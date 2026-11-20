//! Audit records for management writes (tape.audit).

use service_auth::AuditedRoleMapPrincipal;

/// Writes one `tape.audit` record for a successful management write.
/// The record holds the event, the subject, the topic and the name. It never
/// holds a payload, a policy value or a credential.
pub(super) fn record(
    event: &'static str,
    principal: &AuditedRoleMapPrincipal,
    topic: &str,
    name: Option<&str>,
) {
    tracing::info!(
        target: "tape.audit",
        event,
        subject = principal.subject().unwrap_or("anonymous"),
        topic,
        name = name.unwrap_or("-"),
    );
}
