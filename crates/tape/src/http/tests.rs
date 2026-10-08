//! Router-level tests: the data plane end to end over in-process `oneshot`
//! requests, auth on `/admin/backup`, peer-route isolation, and the #2573
//! degraded read-only mode.

use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use axum::Router;

use super::*;
use tape_journal::application::JournalClock;
use tape_replication::raft::TapeRaft;
use tape_shared_kernel::{encode_snapshot, TapeJournal};

#[tokio::test]
async fn append_replay_and_checkpoint_round_trip() {
    let state = AppState::new(TapeJournal::default(), None, 8 * 1024 * 1024);
    let app = router(state);

    let resp = post_json(
        app.clone(),
        "/topics/orders/append",
        &serde_json::json!({ "payload": { "n": 1 } }),
    )
    .await;
    assert_eq!(resp.0, StatusCode::OK);

    let resp = get(app.clone(), "/topics/orders/replay").await;
    assert_eq!(resp.0, StatusCode::OK);
    let body: serde_json::Value = serde_json::from_str(&resp.1).unwrap();
    assert_eq!(body["events"].as_array().unwrap().len(), 1);

    let resp = put_json(
        app.clone(),
        "/topics/orders/consumers/c1/checkpoint",
        &serde_json::json!({ "offset": 1 }),
    )
    .await;
    assert_eq!(resp.0, StatusCode::OK);

    let resp = get(app.clone(), "/topics/orders/consumers/c1/checkpoint").await;
    assert_eq!(resp.0, StatusCode::OK);
    let body: serde_json::Value = serde_json::from_str(&resp.1).unwrap();
    assert_eq!(body["checkpoint"]["offset"], 1);
}

#[tokio::test]
async fn pull_subscription_is_bounded_side_effect_free_and_explicitly_acked() {
    let app = router(AppState::new(TapeJournal::default(), None, 8 * 1024 * 1024));
    for n in 0..2 {
        let response = post_json(
            app.clone(),
            "/topics/orders/append",
            &serde_json::json!({ "payload": { "n": n } }),
        )
        .await;
        assert_eq!(response.0, StatusCode::OK);
    }
    let created = post_json(
        app.clone(),
        "/topics/orders/subscriptions",
        &serde_json::json!({ "name": "audit" }),
    )
    .await;
    assert_eq!(created.0, StatusCode::CREATED);

    let push = post_json(
        app.clone(),
        "/topics/orders/subscriptions",
        &serde_json::json!({
            "name": "webhook",
            "delivery": { "mode": "push", "endpoint": "https://example.invalid" }
        }),
    )
    .await;
    assert_eq!(push.0, StatusCode::BAD_REQUEST, "push is not a Tape mode");

    let first = post_json(
        app.clone(),
        "/topics/orders/subscriptions/audit/pull",
        &serde_json::json!({ "limit": 2 }),
    )
    .await;
    assert_eq!(first.0, StatusCode::OK);
    let first_body: serde_json::Value = serde_json::from_str(&first.1).unwrap();
    assert_eq!(first_body["cursor"], 0);
    assert_eq!(first_body["next_offset"], 2);
    assert_eq!(first_body["events"].as_array().unwrap().len(), 2);

    let repeated = post_json(
        app.clone(),
        "/topics/orders/subscriptions/audit/pull",
        &serde_json::json!({ "limit": 2 }),
    )
    .await;
    let repeated_body: serde_json::Value = serde_json::from_str(&repeated.1).unwrap();
    assert_eq!(repeated_body["cursor"], 0, "pull must not implicitly ack");

    let acked = post_json(
        app.clone(),
        "/topics/orders/subscriptions/audit/ack",
        &serde_json::json!({ "offset": 2 }),
    )
    .await;
    assert_eq!(acked.0, StatusCode::OK);

    let drained = post_json(
        app.clone(),
        "/topics/orders/subscriptions/audit/pull",
        &serde_json::json!({ "limit": 2 }),
    )
    .await;
    let drained_body: serde_json::Value = serde_json::from_str(&drained.1).unwrap();
    assert_eq!(drained_body["cursor"], 2);
    assert!(drained_body["events"].as_array().unwrap().is_empty());

    let stale = post_json(
        app,
        "/topics/orders/subscriptions/audit/ack",
        &serde_json::json!({ "offset": 1 }),
    )
    .await;
    assert_eq!(stale.0, StatusCode::CONFLICT);
}

/// R1: `GET /admin/backup` denies a non-admin principal (403) and
/// streams exactly the snapshot bytes to an admin-on-`*`
/// principal (200), over an in-process `oneshot` request (no real
/// socket — `tests/it/backup.rs` covers the live-HTTP + 401 case).
#[tokio::test]
async fn admin_backup_requires_admin_and_streams_snapshot() {
    use tower::ServiceExt;
    let tokens = serde_json::json!({
        "admin-token": { "subject": "ops", "roles": { "*": "admin" } },
        "reader-token": { "subject": "worker", "roles": { "*": "read" } },
    })
    .to_string();
    let auth = tape_access::AuthConfig::resolve("required", None, Some(&tokens)).unwrap();
    let mut journal = TapeJournal::default();
    journal.append("orders", None, serde_json::json!({"n": 1}), Some(100));
    let state = AppState::with_auth(journal, None, auth, 8 * 1024 * 1024);
    let handle = state.journal_handle();
    let app = router(state);

    let deny = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/admin/backup")
                .header("authorization", "Bearer reader-token")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(deny.status(), StatusCode::FORBIDDEN);

    let ok = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/admin/backup")
                .header("authorization", "Bearer admin-token")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
    let bytes = http_body_util::BodyExt::collect(ok.into_body())
        .await
        .unwrap()
        .to_bytes();
    let expected = encode_snapshot(handle.lock().unwrap().clone(), 0).unwrap();
    assert_eq!(&bytes[..], &expected[..]);
}

#[tokio::test]
async fn secure_peer_mode_does_not_expose_raft_routes_on_public_router() {
    use tower::ServiceExt;

    let journal = Arc::new(Mutex::new(TapeJournal::default()));
    let dir = tempfile::tempdir().unwrap();
    let raft = Arc::new(
        TapeRaft::spawn(
            Arc::clone(&journal),
            dir.path(),
            0,
            raft_runtime::Membership {
                voters: vec![0],
                learners: vec![],
            },
            std::collections::HashMap::new(),
            TapeRaft::host_config(1024),
        )
        .unwrap(),
    );
    let mut state = AppState::new(TapeJournal::default(), None, 8 * 1024 * 1024);
    state.set_raft(raft);

    let response = router_without_raft_routes(state)
        .oneshot(
            axum::http::Request::builder()
                .uri("/raftz")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// Small oneshot helpers so both this module's tests and
// `tests/it/http_transport.rs` share one shape (the integration test drives
// the router over real HTTP instead — these stay unit-level).
pub(crate) async fn get(app: Router, path: &str) -> (StatusCode, String) {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let resp = app
        .oneshot(
            axum::http::Request::builder()
                .uri(path)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

pub(crate) async fn post_json(
    app: Router,
    path: &str,
    body: &serde_json::Value,
) -> (StatusCode, String) {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let resp = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(axum::body::Body::from(serde_json::to_vec(body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

pub(crate) async fn put_json(
    app: Router,
    path: &str,
    body: &serde_json::Value,
) -> (StatusCode, String) {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let resp = app
        .oneshot(
            axum::http::Request::builder()
                .method("PUT")
                .uri(path)
                .header("content-type", "application/json")
                .body(axum::body::Body::from(serde_json::to_vec(body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

mod degraded;
