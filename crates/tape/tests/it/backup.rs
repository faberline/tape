//! Backup surface over a LIVE node (WI #1329): `GET /admin/backup` returns
//! the exact `encode_snapshot` bytes, admin-guarded when auth is
//! required, and — with `--features backup` — `tape::backup::run_backup`
//! ships the bytes to a `core/crates/service-backup` `file://` sink with age
//! retention. Harness: the shared service shell's serve loop on an ephemeral
//! loopback port (the `http_transport.rs` pattern), driven with reqwest.

use std::net::SocketAddr;

use serde_json::json;
use tape::http::{router, AppState};
use tape_journal::application::JournalClock;
use tape_shared_kernel::TapeJournal;

async fn start_server_with_state(state: AppState) -> SocketAddr {
    let app = router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // Tests never signal shutdown; the loop lives for the test process.
    tokio::spawn(service_http::serve(
        listener,
        app,
        std::future::pending::<()>(),
    ));
    addr
}

async fn start_server() -> (SocketAddr, AppState) {
    let state = AppState::new(TapeJournal::default(), None, 8 * 1024 * 1024);
    let addr = start_server_with_state(state.clone()).await;
    (addr, state)
}

/// The endpoint denies a missing/non-admin token and serves an admin-on-`*`
/// token 200, streaming exactly the bytes `encode_snapshot`
/// would produce for the same journal; probes stay tokenless either way.
#[tokio::test]
async fn admin_backup_requires_admin_and_streams_snapshot_over_http() {
    let tokens = json!({
        "admin-token": { "subject": "ops", "roles": { "*": "admin" } },
        "reader-token": { "subject": "worker", "roles": { "*": "read" } },
    })
    .to_string();
    let auth = tape_access::AuthConfig::resolve("required", None, Some(&tokens)).unwrap();
    let journal = TapeJournal::default();
    let state = AppState::with_auth(journal, None, auth, 8 * 1024 * 1024);
    let handle = state.journal_handle();
    handle
        .lock()
        .unwrap()
        .append("orders", None, json!({"n": 1}), Some(100));
    let addr = start_server_with_state(state).await;

    let client = reqwest::Client::new();
    let url = format!("http://{addr}/admin/backup");
    assert_eq!(
        client.get(&url).send().await.unwrap().status(),
        401,
        "tokenless is rejected"
    );
    assert_eq!(
        client
            .get(&url)
            .bearer_auth("reader-token")
            .send()
            .await
            .unwrap()
            .status(),
        403,
        "a cluster-wide admin op needs admin on `*`"
    );
    let resp = client
        .get(&url)
        .bearer_auth("admin-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let bytes = resp.bytes().await.unwrap();
    let expected = tape_shared_kernel::encode_snapshot(handle.lock().unwrap().clone(), 0).unwrap();
    assert_eq!(
        &bytes[..],
        &expected[..],
        "endpoint bytes == encode_snapshot bytes"
    );

    // Probes stay tokenless.
    assert_eq!(
        client
            .get(format!("http://{addr}/healthz"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
}

/// `tape::backup::run_backup` drives the whole path: fetch the snapshot over
/// HTTP, ship it to a `file://` destination sink, and apply age retention
/// (the pre-aged object is pruned, the fresh artifact survives).
#[tokio::test]
async fn run_backup_ships_snapshot_to_local_sink() {
    use service_backup::{BackupDestination, RetentionPolicy};

    let (addr, state) = start_server().await;
    state
        .journal_handle()
        .lock()
        .unwrap()
        .append("orders", None, json!({"n": 1}), Some(100));

    let dir = tempfile::tempdir().unwrap();
    // Pre-age an object so the retention pass has something to prune.
    std::fs::write(dir.path().join("tape-0.json"), b"old").unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

    let dest = BackupDestination::from_uri(&format!("file://{}", dir.path().display())).unwrap();
    let result = tape::backup::run_backup(
        &format!("http://{addr}"),
        None,
        &dest,
        &RetentionPolicy::max_age_seconds(1),
    )
    .await
    .unwrap();

    assert!(result.object.bytes > 0);
    let artifact_path = dir.path().join(&result.object.key);
    assert!(artifact_path.exists(), "artifact written to the sink");
    assert_eq!(result.pruned, 1, "the pre-aged object is pruned");

    let artifact = std::fs::read(&artifact_path).unwrap();
    let expected =
        tape_shared_kernel::encode_snapshot(state.journal_handle().lock().unwrap().clone(), 0)
            .unwrap();
    assert_eq!(&artifact[..], &expected[..]);
}
