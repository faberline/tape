//! The SIGTERM sequence for `tape serve`. One [`ShutdownDeadline`] of
//! `--grace-secs` covers the whole sequence, so the pod's
//! `terminationGracePeriodSeconds` only needs a fixed slack on top of it.
//!
//! 1. `/readyz` turns 503 and the node keeps serving for `--drain-delay-secs`
//!    while endpoints move off it.
//! 2. In replica mode the raft host stops admitting proposals (writes answer
//!    503), hands leadership to a caught-up voter and drains its peer RPCs.
//!    The public listener is still open here: without peer mTLS the raft
//!    routes are mounted on it, so closing it first would cut the handoff.
//! 3. The public listener closes: its lifecycle enters `Draining`, which stops
//!    accepting and tells every open connection to close (GOAWAY on h2,
//!    `Connection: close` on HTTP/1.1), so long-lived peer and keep-alive
//!    connections finish as soon as their in-flight requests do, within the
//!    same deadline. The dedicated peer listener closes gracefully only when
//!    the host's report says that is safe, and is aborted otherwise.

use std::time::Duration;

use anyhow::{Context, Result};
use server_lifecycle::{LifecycleController, ShutdownDeadline};
use tape::http::AppState;
use tape_replication::raft::HostShutdownReport;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;

#[cfg(test)]
mod tests;

/// How long past the deadline to wait for the public HTTP task to report.
/// The server aborts its own connections at the deadline; this only covers
/// the join, and fits inside the kubelet slack on top of `--grace-secs`.
const HTTP_JOIN_SLACK: Duration = Duration::from_secs(1);

/// A spawned server task and the one-shot that asks it to stop accepting.
pub(crate) struct ServerTask<T> {
    pub(crate) shutdown_tx: oneshot::Sender<()>,
    pub(crate) task: JoinHandle<T>,
}

impl<T: Send + 'static> ServerTask<T> {
    /// Spawn `serve`, handing it the future that resolves when this task is
    /// asked to stop.
    pub(crate) fn spawn<F, Fut>(serve: F) -> Self
    where
        F: FnOnce(std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>) -> Fut,
        Fut: std::future::Future<Output = T> + Send + 'static,
    {
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let task = tokio::spawn(serve(Box::pin(async move {
            let _ = shutdown_rx.await;
        })));
        Self { shutdown_tx, task }
    }
}

/// The public HTTP server and the lifecycle that owns its drain. Moving the
/// lifecycle to `Draining` (via [`LifecycleController::shutdown`]) publishes
/// the deadline and asks every connection to close.
pub(crate) struct PublicHttp {
    pub(crate) lifecycle: LifecycleController,
    pub(crate) task: JoinHandle<()>,
}

impl PublicHttp {
    pub(crate) fn spawn(listener: tokio::net::TcpListener, app: axum::Router) -> Self {
        let lifecycle = LifecycleController::serving();
        let task = tokio::spawn(service_http::serve_with_lifecycle(
            listener,
            app,
            service_http::HttpServerOptions::default(),
            lifecycle.clone(),
        ));
        let task = tokio::spawn(async move {
            let Ok(report) = task.await else { return };
            if report.timed_out + report.unfinished > 0 {
                tracing::warn!(
                    event = "http_drain_incomplete",
                    accepted = report.accepted,
                    completed = report.completed,
                    timed_out = report.timed_out,
                    unfinished = report.unfinished,
                    streams_refused = report.streams_refused,
                    "public HTTP connections were cut at the shutdown deadline"
                );
            } else {
                tracing::info!(
                    event = "http_drained",
                    accepted = report.accepted,
                    completed = report.completed,
                    streams_refused = report.streams_refused,
                    "public HTTP server drained"
                );
            }
        });
        Self { lifecycle, task }
    }

    /// Stop accepting and ask open connections to close, draining within
    /// `deadline`.
    pub(crate) async fn close(&self, deadline: ShutdownDeadline) {
        self.lifecycle
            .shutdown(deadline, "shutdown", "raft shut down; closing public HTTP")
            .await;
    }
}

/// Wait for SIGTERM or SIGINT, then run the shutdown sequence within
/// `grace`. Returns an error when the raft host could not finish shutting
/// down in time, so the process exits non-zero.
pub(crate) async fn shutdown_on_signal(
    state: AppState,
    http: PublicHttp,
    peer: Option<ServerTask<Result<()>>>,
    grace: Duration,
    drain_delay: Duration,
) -> Result<()> {
    service_http::wait_shutdown_signal().await;
    let deadline = ShutdownDeadline::from_now(grace, Duration::ZERO)
        .expect("a zero reserve fits any shutdown grace");
    tracing::info!(
        event = "shutdown_started",
        grace_ms = grace.as_millis() as u64,
        drain_delay_ms = drain_delay.as_millis() as u64,
        "shutdown signal received; draining"
    );
    state.start_drain();
    tokio::time::sleep_until(drain_until(deadline, drain_delay)).await;

    let (raft_result, close_safe) = match state.raft() {
        Some(raft) => {
            raft.quiesce_proposals();
            let report = raft.shutdown_within(deadline).await;
            log_raft_report(&report, deadline);
            let close_safe = report.peer_listener_close_safe;
            (
                report.into_result().context("raft shutdown is incomplete"),
                close_safe,
            )
        }
        None => (Ok(()), true),
    };

    http.close(deadline).await;
    let peer_result = match peer {
        Some(peer) => finish_peer_listener(close_safe, peer, deadline).await,
        None => Ok(()),
    };
    finish_http(http.task, deadline).await;
    raft_result.and(peer_result)
}

/// When the readiness drain ends: `drain_delay` from now, but never past the
/// deadline.
pub(crate) fn drain_until(deadline: ShutdownDeadline, drain_delay: Duration) -> Instant {
    (Instant::now() + drain_delay).min(deadline.expires_at)
}

/// Close the raft peer listener. `close_safe` comes from the host's shutdown
/// report: only then may in-flight peer connections finish, within the
/// deadline. Otherwise the listener is aborted at once, without a graceful
/// close first, so it cannot outlive the host's own deadline.
pub(crate) async fn finish_peer_listener(
    close_safe: bool,
    peer: ServerTask<Result<()>>,
    deadline: ShutdownDeadline,
) -> Result<()> {
    let ServerTask {
        shutdown_tx,
        mut task,
    } = peer;
    if !close_safe {
        task.abort();
        tracing::info!(
            event = "raft_peer_listener_aborted",
            reason = "raft shutdown incomplete",
            "raft peer listener aborted"
        );
        return Ok(());
    }
    let _ = shutdown_tx.send(());
    match tokio::time::timeout_at(deadline.expires_at, &mut task).await {
        Ok(joined) => {
            tracing::info!(
                event = "raft_peer_listener_closed",
                "raft peer listener closed"
            );
            joined.context("raft peer listener task panicked")?
        }
        Err(_) => {
            task.abort();
            tracing::info!(
                event = "raft_peer_listener_aborted",
                reason = "deadline expired",
                "raft peer listener aborted"
            );
            Ok(())
        }
    }
}

/// Wait for the public HTTP server to finish draining. It bounds its own
/// connections by the deadline; the task is aborted only if it overruns that
/// by [`HTTP_JOIN_SLACK`].
pub(crate) async fn finish_http(mut task: JoinHandle<()>, deadline: ShutdownDeadline) {
    if tokio::time::timeout_at(deadline.expires_at + HTTP_JOIN_SLACK, &mut task)
        .await
        .is_err()
    {
        task.abort();
        tracing::warn!(
            event = "http_drain_aborted",
            "shutdown grace expired before the HTTP server drained"
        );
    }
}

fn log_raft_report(report: &HostShutdownReport, deadline: ShutdownDeadline) {
    tracing::info!(
        event = "raft_shutdown",
        handoff = ?report.handoff,
        incomplete_phase = ?report.incomplete_phase,
        peer_listener_close_safe = report.peer_listener_close_safe,
        storage_failure = ?report.storage_failure,
        shutdown_budget_ms = deadline.total.as_millis() as u64,
        "raft host shut down"
    );
}
