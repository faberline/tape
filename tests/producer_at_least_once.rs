// HANDWRITE-BEGIN gap="missing-generator:unit-test:2548-producer-at-least-once" tracker="#2548" reason="Guard test pinning current at-least-once append behavior and key propagation."
//! Guard test pinning Tape's current producer delivery semantics:
//! Appending twice with identical key and payload produces two distinct events
//! with distinct offsets, both of which are preserved and returned on replay with
//! their original key.
//!
//! Note: This test pins existing behavior as a guard rather than reproducing a
//! regression. It is expected to go red if deduplication is ever introduced
//! without updating docs/delivery-semantics.md.

use std::net::SocketAddr;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::json;
use tape::server::{router, AppState};
use tape::wal::{CommitCoordinator, WalStore};
use tape::TapeEvent;

async fn start_server_with_state(state: AppState) -> SocketAddr {
    let app = router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(service_http::serve(
        listener,
        app,
        std::future::pending::<()>(),
    ));
    addr
}

fn wal_backed_state(dir: &std::path::Path) -> AppState {
    let (store, journal) = WalStore::open(dir).unwrap();
    let state = AppState::new(journal, None, 8 * 1024 * 1024);
    let coordinator = CommitCoordinator::spawn(store, state.journal_handle());
    state.with_wal(Arc::new(coordinator))
}

#[derive(Deserialize)]
struct ReplayBody {
    events: Vec<TapeEvent>,
}

#[tokio::test]
async fn producer_append_is_at_least_once_and_key_is_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let state = wal_backed_state(dir.path());
    let addr = start_server_with_state(state).await;
    let client = reqwest::Client::new();

    let topic = "idempotency-test";
    let append_url = format!("http://{addr}/topics/{topic}/append");
    let replay_url = format!("http://{addr}/topics/{topic}/replay");

    let payload = json!({
        "key": "test-dedup-key-123",
        "payload": { "action": "payment", "amount": 100 }
    });

    // 1. Both appends return 200.
    let resp1 = client
        .post(&append_url)
        .json(&payload)
        .send()
        .await
        .expect("first append request failed");
    assert_eq!(resp1.status(), reqwest::StatusCode::OK);
    let event1: TapeEvent = resp1.json().await.expect("parse event1 JSON");

    let resp2 = client
        .post(&append_url)
        .json(&payload)
        .send()
        .await
        .expect("second append request failed");
    assert_eq!(resp2.status(), reqwest::StatusCode::OK);
    let event2: TapeEvent = resp2.json().await.expect("parse event2 JSON");

    // 2. The two returned offset values are distinct.
    assert_ne!(
        event1.offset, event2.offset,
        "Retried append with identical key must yield distinct offsets"
    );

    // 3. Replay of the topic from offset 0 returns both events.
    let replay_resp = client
        .get(&replay_url)
        .send()
        .await
        .expect("replay request failed");
    assert_eq!(replay_resp.status(), reqwest::StatusCode::OK);

    let body: ReplayBody = replay_resp.json().await.expect("parse replay body JSON");
    assert_eq!(
        body.events.len(),
        2,
        "Replay must return both appended events without server-side deduplication"
    );
    assert_eq!(body.events[0].offset, event1.offset);
    assert_eq!(body.events[1].offset, event2.offset);

    // 4. Both replayed events carry the key value that was sent.
    assert_eq!(
        body.events[0].key.as_deref(),
        Some("test-dedup-key-123"),
        "First replayed event must preserve key"
    );
    assert_eq!(
        body.events[1].key.as_deref(),
        Some("test-dedup-key-123"),
        "Second replayed event must preserve key"
    );
}
