//! The background poller that publishes whether the raft group has a leader.

use std::sync::Arc;
use std::time::Duration;

use tape_journal::application::TapeMetrics;
use tape_replication::raft::TapeRaft;

/// #2579: periodically publish the latest raft-leader observation into the
/// metrics gauge.
///
/// `/metrics` renders synchronously (`MetricsProvider::render_metrics`) and
/// `TapeRaft::leader` is asynchronous, so the value cannot be read at scrape
/// time. It has to be pushed by a background task. This task is spawned only
/// from the raft branch of `serve`, after `state.set_raft(raft)`; that
/// load-bearing choice keeps the gauge at `NOT_RAFT` everywhere else, so
/// `TapeMetrics::render` omits the series and `TapeRaftLeaderAbsent` cannot
/// fire on single-node deployments. Spawning it unconditionally would make
/// every non-raft tape alert forever. `TAPE_RAFT_LEADER_POLL_SECS` (default 5)
/// controls the cadence; a non-positive or unparseable value falls back to the
/// default.
pub(crate) fn spawn_raft_leader_poller(metrics: Arc<TapeMetrics>, raft: Arc<TapeRaft>) {
    let poll_secs: u64 = std::env::var("TAPE_RAFT_LEADER_POLL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&v| v > 0)
        .unwrap_or(5);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(poll_secs));
        ticker.tick().await; // discard the immediate first fire
        let mut last_known = None;
        loop {
            let known = raft.leader().await.is_some();
            metrics.set_raft_leader_known(known);
            if let Some(previous) = last_known {
                if previous && !known {
                    tracing::warn!("raft leader lost; no elected leader reported by the raft host");
                } else if !previous && known {
                    tracing::info!("raft leader discovered; leader election has produced a leader");
                }
            }
            last_known = Some(known);
            ticker.tick().await;
        }
    });
}
