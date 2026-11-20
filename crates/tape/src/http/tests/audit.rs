//! Audit records for management writes and failed authentication.

use std::io::Write;
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use axum::Router;

use super::*;

/// Captures everything the thread-local subscriber writes.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl Capture {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }

    /// Lines of the two audit targets only.
    fn lines(&self) -> Vec<String> {
        self.text()
            .lines()
            .filter(|line| line.contains("tape.audit") || line.contains("service_auth.audit"))
            .map(str::to_owned)
            .collect()
    }
}

fn capture_subscriber() -> (Capture, tracing::subscriber::DefaultGuard) {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    (capture, guard)
}

fn app() -> Router {
    let tokens = serde_json::json!({
        "writer-token": { "subject": "alice", "roles": { "*": "write" } },
    })
    .to_string();
    let auth = tape_access::AuthConfig::resolve("required", None, Some(&tokens)).unwrap();
    let state = AppState::with_auth(TapeJournal::default(), None, auth, 8 * 1024 * 1024);
    router(state)
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<serde_json::Value>,
) -> StatusCode {
    use tower::ServiceExt;
    let mut req = axum::http::Request::builder().method(method).uri(path);
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    let body = match body {
        Some(value) => {
            req = req.header("content-type", "application/json");
            axum::body::Body::from(value.to_string())
        }
        None => axum::body::Body::empty(),
    };
    app.clone()
        .oneshot(req.body(body).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn subscription_create_and_delete_each_make_one_audit_record() {
    let (capture, _guard) = capture_subscriber();
    let app = app();
    let appended = call(
        &app,
        "POST",
        "/topics/orders/append",
        Some("writer-token"),
        Some(serde_json::json!({"payload": {"n": 1}})),
    )
    .await;
    assert_eq!(appended, StatusCode::OK);

    let created = call(
        &app,
        "POST",
        "/topics/orders/subscriptions",
        Some("writer-token"),
        Some(serde_json::json!({"name": "billing"})),
    )
    .await;
    assert_eq!(created, StatusCode::CREATED);

    let deleted = call(
        &app,
        "DELETE",
        "/topics/orders/subscriptions/billing",
        Some("writer-token"),
        None,
    )
    .await;
    assert_eq!(deleted, StatusCode::OK);

    let lines = capture.lines();
    let created_lines: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains("subscription_created"))
        .collect();
    let deleted_lines: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains("subscription_deleted"))
        .collect();
    assert_eq!(created_lines.len(), 1, "lines: {lines:?}");
    assert_eq!(deleted_lines.len(), 1, "lines: {lines:?}");
    for line in [created_lines[0], deleted_lines[0]] {
        assert!(line.contains("subject=\"alice\""), "line: {line}");
        assert!(line.contains("topic=\"orders\""), "line: {line}");
        assert!(line.contains("name=\"billing\""), "line: {line}");
    }
}

#[tokio::test]
async fn retention_change_makes_one_record_without_policy_values() {
    let (capture, _guard) = capture_subscriber();
    let app = app();
    let appended = call(
        &app,
        "POST",
        "/topics/orders/append",
        Some("writer-token"),
        Some(serde_json::json!({"payload": {"n": 1}})),
    )
    .await;
    assert_eq!(appended, StatusCode::OK);

    let put = call(
        &app,
        "PUT",
        "/topics/orders/retention",
        Some("writer-token"),
        Some(serde_json::json!({
            "min_offset": 0,
            "max_age_seconds": null,
            "protected_consumers": ["billing-consumer"]
        })),
    )
    .await;
    assert_eq!(put, StatusCode::OK);

    let lines = capture.lines();
    let changed: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains("retention_changed"))
        .collect();
    assert_eq!(changed.len(), 1, "lines: {lines:?}");
    assert!(
        changed[0].contains("subject=\"alice\""),
        "line: {}",
        changed[0]
    );
    assert!(
        changed[0].contains("topic=\"orders\""),
        "line: {}",
        changed[0]
    );
    assert!(
        !capture.text().contains("billing-consumer"),
        "policy value leaked into a record"
    );
}

#[tokio::test]
async fn append_makes_no_audit_record() {
    let (capture, _guard) = capture_subscriber();
    let app = app();
    let appended = call(
        &app,
        "POST",
        "/topics/orders/append",
        Some("writer-token"),
        Some(serde_json::json!({"payload": {"secret": "s3cr3t-payload"}})),
    )
    .await;
    assert_eq!(appended, StatusCode::OK);

    assert!(
        !capture.lines().iter().any(|l| l.contains("tape.audit")),
        "append must not write a tape.audit record: {:?}",
        capture.lines()
    );
    let text = capture.text();
    assert!(!text.contains("s3cr3t-payload"), "payload leaked: {text}");
    assert!(!text.contains("writer-token"), "token leaked: {text}");
}

#[tokio::test]
async fn failed_authentication_makes_a_record_without_the_token() {
    let (capture, _guard) = capture_subscriber();
    let app = app();
    let missing = call(&app, "GET", "/topics/orders/replay", None, None).await;
    assert_eq!(missing, StatusCode::UNAUTHORIZED);
    let unknown = call(
        &app,
        "GET",
        "/topics/orders/replay",
        Some("wrong-secret-token"),
        None,
    )
    .await;
    assert_eq!(unknown, StatusCode::UNAUTHORIZED);

    let denies: Vec<String> = capture
        .lines()
        .into_iter()
        .filter(|l| l.contains("service_auth.audit") && l.contains("authorization_decision"))
        .collect();
    assert!(
        denies.iter().any(|l| l.contains("MissingBearer")),
        "no MissingBearer record: {denies:?}"
    );
    assert!(
        denies.iter().any(|l| l.contains("UnknownBearer")),
        "no UnknownBearer record: {denies:?}"
    );
    assert!(
        !capture.text().contains("wrong-secret-token"),
        "bearer token leaked into a record"
    );
}
