//! The data plane answers authorization before it reads a subscription create
//! body. This file pins that boundary over the real HTTP surface: a tokenless
//! or unknown-token create gets `401`, a read-only token gets `403`, even when
//! the body carries an out-of-range `ackDeadlineSeconds`, and a refused create
//! leaves no subscription behind.
//!
//! The `ackDeadlineSeconds` configuration tests were removed with #11: that
//! scope is superseded by the cursor model (#12 and #13, pure pull).
//! `ACK_FIELD` is the name of the out-of-range probe field in the remaining
//! test.

use std::net::SocketAddr;

use serde_json::json;

use tape::http::{router, AppState};
use tape_access::AuthConfig;
use tape_shared_kernel::TapeJournal;

/// The wire name is Pub/Sub's, per the work item's frozen decisions: camelCase
/// on the JSON surface, not the crate's usual snake_case.
const ACK_FIELD: &str = "ackDeadlineSeconds";

const BODY_LIMIT: usize = 8 * 1024 * 1024;

/// producer holds `write` on the fixture topic, worker holds `read`, and root
/// is a wildcard admin. Mirrors `tests/it/service_auth.rs`'s registry so the two
/// files describe one authorization model.
const REGISTRY: &str = r#"{
    "writer-token": {"subject": "producer", "roles": {"acks": "write"}},
    "reader-token": {"subject": "worker", "roles": {"acks": "read"}},
    "admin-token": {"subject": "root", "roles": {"*": "admin"}}
}"#;

// --------------------------------------------------------------------------
// harness
// --------------------------------------------------------------------------

/// The same server with the shared bearer-token contract in `required` mode,
/// resolved through the real registry-file loader.
async fn start_server_with_auth() -> SocketAddr {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("token-registry.json");
    std::fs::write(&path, REGISTRY).unwrap();
    let cfg = AuthConfig::resolve("required", Some(path.to_str().unwrap()), None).unwrap();
    // Short-lived test process; keep the registry file alive for the config.
    std::mem::forget(dir);
    spawn(AppState::with_auth(
        TapeJournal::default(),
        None,
        cfg,
        BODY_LIMIT,
    ))
    .await
}

async fn spawn(state: AppState) -> SocketAddr {
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

fn url(addr: SocketAddr, path: &str) -> String {
    format!("http://{addr}{path}")
}

/// One real event on the topic, so every subscription below is a cursor over
/// something rather than a name in an empty map.
async fn seed_topic(client: &reqwest::Client, addr: SocketAddr, topic: &str, token: Option<&str>) {
    let mut req = client
        .post(url(addr, &format!("/topics/{topic}/append")))
        .json(&json!({ "payload": { "seeded": true } }));
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }
    let resp = req.send().await.unwrap();
    assert!(
        resp.status().is_success(),
        "seeding topic `{topic}` must succeed, got {} {}",
        resp.status(),
        resp.text().await.unwrap_or_default()
    );
}

struct Answer {
    status: reqwest::StatusCode,
    body: String,
}

async fn send(req: reqwest::RequestBuilder) -> Answer {
    let resp = req.send().await.unwrap();
    let status = resp.status();
    let body = resp.text().await.unwrap();
    Answer { status, body }
}

/// `POST /topics/{topic}/subscriptions` with a caller-supplied body, sent as
/// raw bytes so a case can post a shape `serde_json::json!` cannot build.
async fn create_raw(
    client: &reqwest::Client,
    addr: SocketAddr,
    topic: &str,
    body: &str,
    token: Option<&str>,
) -> Answer {
    let mut req = client
        .post(url(addr, &format!("/topics/{topic}/subscriptions")))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_string());
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }
    send(req).await
}

async fn read(
    client: &reqwest::Client,
    addr: SocketAddr,
    topic: &str,
    name: &str,
    token: Option<&str>,
) -> Answer {
    let mut req = client.get(url(addr, &format!("/topics/{topic}/subscriptions/{name}")));
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }
    send(req).await
}

// --------------------------------------------------------------------------
// security: the authorization boundary
// --------------------------------------------------------------------------

/// The authorization boundary answers before `ackDeadlineSeconds` is read.
///
/// **This case is green against the current tree and must stay green.** It is
/// this file's negative control against the most likely way to implement the
/// rest of it: replacing the handler's `body: axum::body::Bytes` with a typed
/// `Json<SubscriptionCreateRequest>` extractor. Axum runs an extractor before
/// the handler body, so that edit moves validation *ahead* of
/// `crate::auth::authorize` and hands a read-only caller a `400` describing
/// the allowed range — a validation oracle on a topic the caller has no write
/// grant for. The reader-token row is the one that catches it: the anonymous
/// and unknown-token rows are answered `401` by the data plane's `route_layer`
/// before any extractor runs, so they hold under that edit too and only
/// document the boundary. Every other case in this file would still be green
/// after that edit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authorization_is_answered_before_ack_deadline_seconds_is_read() {
    let addr = start_server_with_auth().await;
    let client = reqwest::Client::new();
    seed_topic(&client, addr, "acks", Some("admin-token")).await;

    // Deliberately out of range: the only reason to answer anything other than
    // 401/403 here is having validated the field before checking the caller.
    let body = format!(r#"{{"name":"probe","{ACK_FIELD}":601}}"#);

    let anonymous = create_raw(&client, addr, "acks", &body, None).await;
    assert_eq!(
        anonymous.status.as_u16(),
        401,
        "a tokenless create must be refused by the auth boundary, not by \
         `{ACK_FIELD}` validation; got {} body {:?}",
        anonymous.status,
        anonymous.body
    );

    let unknown = create_raw(&client, addr, "acks", &body, Some("not-a-real-token")).await;
    assert_eq!(
        unknown.status.as_u16(),
        401,
        "an unknown token must be refused by the auth boundary; got {} body {:?}",
        unknown.status,
        unknown.body
    );

    let read_only = create_raw(&client, addr, "acks", &body, Some("reader-token")).await;
    assert_eq!(
        read_only.status.as_u16(),
        403,
        "a read-only token must be refused by authorization, not by \
         `{ACK_FIELD}` validation; got {} body {:?}",
        read_only.status,
        read_only.body
    );

    // Fail closed: none of the three refused calls may have written anything.
    let fetched = read(&client, addr, "acks", "probe", Some("admin-token")).await;
    assert_eq!(
        fetched.status.as_u16(),
        404,
        "no refused create may leave a subscription behind; got {} body {:?}",
        fetched.status,
        fetched.body
    );
}
