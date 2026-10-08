//! Per-topic retention policy.

use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use service_auth::{AuditedRoleMapPrincipal, Role};

use super::dto::RetentionGetResponse;
use super::errors::parse_body;
use crate::application::JournalService;
use tape_access::authorize;
use tape_shared_kernel::RetentionPolicy;

#[utoipa::path(
    get,
    path = "/topics/{topic}/retention",
    params(("topic" = String, Path, description = "Topic name")),
    responses((status = 200, description = "Topic retention policy", body = RetentionGetResponse))
)]
pub async fn retention_get(
    State(service): State<JournalService>,
    Extension(principal): Extension<AuditedRoleMapPrincipal>,
    Path(topic): Path<String>,
) -> Response {
    if let Err(deny) = authorize(&principal, &topic, Role::Read) {
        return deny.into_response();
    }
    let policy = service.retention(&topic);
    (StatusCode::OK, Json(RetentionGetResponse { policy })).into_response()
}

#[utoipa::path(
    put,
    path = "/topics/{topic}/retention",
    params(("topic" = String, Path, description = "Topic name")),
    request_body = RetentionPolicy,
    responses(
        (status = 200, description = "Applied policy and compaction result", body = RetentionOutcome),
        (status = 507, description = "Node is in ENOSPC degraded read-only mode (storage_full)")
    )
)]
pub async fn retention_put(
    State(service): State<JournalService>,
    Extension(principal): Extension<AuditedRoleMapPrincipal>,
    Path(topic): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    if let Err(deny) = authorize(&principal, &topic, Role::Write) {
        return deny.into_response();
    }
    // Same reasoning as `subscription_delete`: applying retention compacts the
    // journal, but the persist that records it still stages a full second copy
    // first. Freeing space is not a way around a disk that is already full.
    if let Err(full) = service.storage_writable() {
        return full.into_response();
    }
    let policy: RetentionPolicy = match parse_body(&body) {
        Ok(policy) => policy,
        Err(bad) => return bad.into_response(),
    };
    match service.put_retention(topic, policy).await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => error.into_response(),
    }
}
