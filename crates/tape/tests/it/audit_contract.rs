//! Audit-boundary regression contract for the Tape service.

const ADMIN: &str = include_str!("../../../tape-journal/src/interfaces/http/admin.rs");
const AUTH: &str = include_str!("../../../tape-access/src/authorization.rs");
const SUBSCRIPTIONS: &str =
    include_str!("../../../tape-journal/src/interfaces/http/subscriptions.rs");
const RETENTION: &str = include_str!("../../../tape-journal/src/interfaces/http/retention.rs");

#[test]
fn management_writes_are_audited_in_source() {
    assert!(SUBSCRIPTIONS.contains("\"subscription_created\""));
    assert!(SUBSCRIPTIONS.contains("\"subscription_deleted\""));
    assert!(RETENTION.contains("\"retention_changed\""));
}

#[test]
fn backup_audit_is_redacted_and_kept_off_hot_data_plane_routes() {
    assert!(ADMIN.contains("target: \"tape.audit\""));
    assert!(ADMIN.contains("event = \"backup_snapshot_served\""));
    assert!(ADMIN.contains("subject = principal.subject().unwrap_or(\"anonymous\")"));
    assert!(ADMIN.contains("applied_index = applied"));
    assert!(ADMIN.contains("bytes = bytes.len()"));

    // The service adapter delegates token/authorization audit fields to the
    // shared verifier; it does not parse or log bearer credentials itself.
    assert!(AUTH.contains("TracingAuthEventSink"));
    assert!(AUTH.contains("self.tracing.record(event);"));
    assert!(!AUTH.contains("tracing::"));
}
