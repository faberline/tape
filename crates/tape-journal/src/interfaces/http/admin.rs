//! Cluster-wide admin operations.

use axum::extract::{Extension, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use service_auth::{AuditedRoleMapPrincipal, Role};

use crate::application::JournalService;
use tape_access::authorize;

/// `GET /admin/backup` — a consistent snapshot of the whole journal for
/// backup runners (#1329): the EXACT bytes the raft state machine's snapshot
/// produces (the whole journal + the applied index; 0 on a raft-less single
/// node). A
/// cluster-wide admin op: requires `admin` on `*` when auth is required.
/// Restore = feed the bytes to `TapeStateMachine::restore` on a fresh node
/// (the existing raft-side merge path); no restore CLI verb is added here.
#[utoipa::path(
    get,
    path = "/admin/backup",
    responses((status = 200, description = "JournalSnapshot JSON { up_to, journal } — the whole journal at the applied raft index"))
)]
pub async fn admin_backup(
    State(service): State<JournalService>,
    Extension(principal): Extension<AuditedRoleMapPrincipal>,
) -> Response {
    if let Err(deny) = authorize(&principal, "*", Role::Admin) {
        return deny.into_response();
    }
    match service.backup_snapshot() {
        Ok((applied, bytes)) => {
            // Audit only the low-frequency management operation. Append and
            // consumer checkpoint traffic is deliberately not duplicated into
            // logs: its durable, payload-free audit trail is the Tape journal
            // itself, while credentials/denials are already emitted through
            // the shared service-auth redacted audit sink.
            tracing::info!(
                target: "tape.audit",
                event = "backup_snapshot_served",
                subject = principal.subject().unwrap_or("anonymous"),
                applied_index = applied,
                bytes = bytes.len(),
            );
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                bytes,
            )
                .into_response()
        }
        Err(error) => error.into_response(),
    }
}
