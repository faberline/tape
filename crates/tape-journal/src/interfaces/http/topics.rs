//! Topic append, replay, and consumer checkpoints.

use axum::extract::{Extension, Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use service_auth::{AuditedRoleMapPrincipal, Role};

use super::dto::{
    AppendRequest, CheckpointPutRequest, CheckpointResponse, ReplayQuery, ReplayResponse,
};
use super::errors::parse_body;
use crate::application::{JournalService, REPLAY_CONTENT_TYPE};
use tape_access::authorize;

/// `POST /topics/{topic}/append` — append one event envelope to the topic
/// journal.
#[utoipa::path(
    post,
    path = "/topics/{topic}/append",
    params(("topic" = String, Path, description = "Topic name")),
    request_body = AppendRequest,
    responses(
        (status = 200, description = "The appended event", body = TapeEvent),
        (status = 507, description = "Node is in ENOSPC degraded read-only mode (storage_full)")
    )
)]
pub async fn append(
    State(service): State<JournalService>,
    Extension(principal): Extension<AuditedRoleMapPrincipal>,
    Path(topic): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    if let Err(deny) = authorize(&principal, &topic, Role::Write) {
        return deny.into_response();
    }
    if let Err(full) = service.storage_writable() {
        return full.into_response();
    }
    let req: AppendRequest = match parse_body(&body) {
        Ok(req) => req,
        Err(bad) => return bad.into_response(),
    };
    match service
        .append(topic, req.key, req.payload, req.timestamp_ms)
        .await
    {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => error.into_response(),
    }
}

/// `GET /topics/{topic}/replay` — replay topic history by offset or
/// timestamp.
#[utoipa::path(
    get,
    path = "/topics/{topic}/replay",
    params(
        ("topic" = String, Path, description = "Topic name"),
        ("from_offset" = Option<u64>, Query, description = "First offset to include"),
        ("from_timestamp_ms" = Option<u64>, Query, description = "First event timestamp to include"),
        ("limit" = Option<usize>, Query, description = "Maximum number of events to return"),
    ),
    responses((status = 200, description = "Matching events", body = ReplayResponse))
)]
pub async fn replay(
    State(service): State<JournalService>,
    Extension(principal): Extension<AuditedRoleMapPrincipal>,
    Path(topic): Path<String>,
    Query(q): Query<ReplayQuery>,
) -> Response {
    if let Err(deny) = authorize(&principal, &topic, Role::Read) {
        return deny.into_response();
    }
    let events = service.replay(&topic, q.from_offset, q.from_timestamp_ms, q.limit);
    (StatusCode::OK, Json(ReplayResponse { events })).into_response()
}

/// `GET /topics/{topic}/replay/stream` — compact read-only h2c bulk replay.
/// The topic is carried by the path once; each frame retains offset, event
/// time, optional key, and opaque JSON payload bytes.
#[utoipa::path(
    get,
    path = "/topics/{topic}/replay/stream",
    params(
        ("topic" = String, Path, description = "Topic name"),
        ("from_offset" = Option<u64>, Query, description = "First offset to include"),
        ("from_timestamp_ms" = Option<u64>, Query, description = "First event timestamp to include"),
        ("limit" = Option<usize>, Query, description = "Maximum number of events to return"),
    ),
    responses((status = 200, description = "Length-framed Tape replay stream", content_type = "application/vnd.tape.replay.v1", body = Vec<u8>))
)]
pub async fn replay_stream(
    State(service): State<JournalService>,
    Extension(principal): Extension<AuditedRoleMapPrincipal>,
    Path(topic): Path<String>,
    Query(q): Query<ReplayQuery>,
) -> Response {
    if let Err(deny) = authorize(&principal, &topic, Role::Read) {
        return deny.into_response();
    }
    match service.replay_stream(&topic, q.from_offset, q.from_timestamp_ms, q.limit) {
        Ok(body) => (
            [
                (header::CONTENT_TYPE, REPLAY_CONTENT_TYPE),
                (header::CACHE_CONTROL, "no-store"),
            ],
            body,
        )
            .into_response(),
        Err(error) => error.into_response(),
    }
}

/// `GET /topics/{topic}/consumers/{consumer}/checkpoint` — read a consumer
/// checkpoint.
#[utoipa::path(
    get,
    path = "/topics/{topic}/consumers/{consumer}/checkpoint",
    params(
        ("topic" = String, Path, description = "Topic name"),
        ("consumer" = String, Path, description = "Consumer name"),
    ),
    responses((status = 200, description = "The consumer's checkpoint, if any", body = CheckpointResponse))
)]
pub async fn checkpoint_get(
    State(service): State<JournalService>,
    Extension(principal): Extension<AuditedRoleMapPrincipal>,
    Path((topic, consumer)): Path<(String, String)>,
) -> Response {
    if let Err(deny) = authorize(&principal, &topic, Role::Read) {
        return deny.into_response();
    }
    let checkpoint = service.checkpoint(&topic, &consumer);
    (StatusCode::OK, Json(CheckpointResponse { checkpoint })).into_response()
}

/// `PUT /topics/{topic}/consumers/{consumer}/checkpoint` — advance a
/// consumer checkpoint.
#[utoipa::path(
    put,
    path = "/topics/{topic}/consumers/{consumer}/checkpoint",
    params(
        ("topic" = String, Path, description = "Topic name"),
        ("consumer" = String, Path, description = "Consumer name"),
    ),
    request_body = CheckpointPutRequest,
    responses(
        (status = 200, description = "The advanced checkpoint", body = ConsumerCheckpoint),
        (status = 409, description = "Stale or beyond-end checkpoint offset"),
        (status = 507, description = "Node is in ENOSPC degraded read-only mode (storage_full)")
    )
)]
pub async fn checkpoint_put(
    State(service): State<JournalService>,
    Extension(principal): Extension<AuditedRoleMapPrincipal>,
    Path((topic, consumer)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> Response {
    if let Err(deny) = authorize(&principal, &topic, Role::Read) {
        return deny.into_response();
    }
    if let Err(full) = service.storage_writable() {
        return full.into_response();
    }
    let req: CheckpointPutRequest = match parse_body(&body) {
        Ok(req) => req,
        Err(bad) => return bad.into_response(),
    };
    match service.put_checkpoint(topic, consumer, req.offset).await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => error.into_response(),
    }
}
