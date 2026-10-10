use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use server_lifecycle::ShutdownDeadline;

use super::{drain_until, finish_http, finish_peer_listener, PublicHttp, ServerTask};

/// A peer listener stand-in that records whether it saw the graceful-close
/// request, and then either returns or keeps running.
fn peer(closed: Arc<AtomicBool>, exits_on_close: bool) -> ServerTask<Result<()>> {
    ServerTask::spawn(move |stop| async move {
        stop.await;
        closed.store(true, Ordering::SeqCst);
        if !exits_on_close {
            std::future::pending::<()>().await;
        }
        Ok(())
    })
}

#[tokio::test]
async fn unsafe_close_aborts_without_a_graceful_close_first() {
    let closed = Arc::new(AtomicBool::new(false));
    let peer = peer(closed.clone(), true);
    let deadline = ShutdownDeadline::from_now(Duration::from_secs(5), Duration::ZERO).unwrap();
    let started = tokio::time::Instant::now();

    finish_peer_listener(false, peer, deadline).await.unwrap();

    assert!(started.elapsed() < Duration::from_secs(1));
    tokio::task::yield_now().await;
    assert!(
        !closed.load(Ordering::SeqCst),
        "an incomplete raft shutdown must not ask the peer listener to close gracefully"
    );
}

#[tokio::test]
async fn safe_close_waits_for_the_listener_to_finish() {
    let closed = Arc::new(AtomicBool::new(false));
    let peer = peer(closed.clone(), true);
    let deadline = ShutdownDeadline::from_now(Duration::from_secs(5), Duration::ZERO).unwrap();

    finish_peer_listener(true, peer, deadline).await.unwrap();

    assert!(closed.load(Ordering::SeqCst));
}

#[tokio::test]
async fn safe_close_aborts_a_listener_that_outlives_the_deadline() {
    let closed = Arc::new(AtomicBool::new(false));
    let peer = peer(closed.clone(), false);
    let deadline = ShutdownDeadline::from_now(Duration::from_millis(100), Duration::ZERO).unwrap();
    let started = tokio::time::Instant::now();

    finish_peer_listener(true, peer, deadline).await.unwrap();

    assert!(closed.load(Ordering::SeqCst));
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn safe_close_reports_a_listener_error() {
    let peer = ServerTask::spawn(|stop| async move {
        stop.await;
        anyhow::bail!("peer serve failed")
    });
    let deadline = ShutdownDeadline::from_now(Duration::from_secs(5), Duration::ZERO).unwrap();

    let error = finish_peer_listener(true, peer, deadline)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("peer serve failed"));
}

#[tokio::test]
async fn drain_delay_is_capped_by_the_deadline() {
    let deadline = ShutdownDeadline::from_now(Duration::from_secs(2), Duration::ZERO).unwrap();

    assert_eq!(
        drain_until(deadline, Duration::from_secs(60)),
        deadline.expires_at
    );
    assert!(drain_until(deadline, Duration::ZERO) < deadline.expires_at);
}

/// An idle HTTP/1.1 keep-alive connection must not hold the public server
/// open until the deadline: closing the listener tells it to close too.
#[tokio::test]
async fn public_http_closes_idle_keep_alive_connections_before_the_deadline() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = axum::Router::new().route("/", axum::routing::get(|| async { "ok" }));
    let http = PublicHttp::spawn(listener, app);

    let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
    client
        .write_all(b"GET / HTTP/1.1\r\nhost: tape\r\n\r\n")
        .await
        .unwrap();
    let mut buf = [0u8; 1024];
    let read = client.read(&mut buf).await.unwrap();
    assert!(String::from_utf8_lossy(&buf[..read]).starts_with("HTTP/1.1 200"));

    let deadline = ShutdownDeadline::from_now(Duration::from_secs(10), Duration::ZERO).unwrap();
    let started = tokio::time::Instant::now();
    http.close(deadline).await;
    finish_http(http.task, deadline).await;

    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the HTTP drain ran {:?}, toward the deadline",
        started.elapsed()
    );
    let eof = tokio::time::timeout(Duration::from_secs(1), client.read(&mut buf))
        .await
        .expect("the server closed the keep-alive connection")
        .unwrap();
    assert_eq!(eof, 0, "the keep-alive connection is closed by the server");
}
