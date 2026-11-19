//! Pull subscriptions: create, list, get, delete, pull, and ack.

use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use service_auth::{AuditedRoleMapPrincipal, Role};

use super::dto::{
    SubscriptionAckRequest, SubscriptionCreateRequest, SubscriptionListResponse,
    SubscriptionPullRequest, SubscriptionSeekRequest,
};
use super::errors::parse_body;
use crate::application::JournalService;
use tape_access::authorize;
use tape_shared_kernel::{ConsumerCheckpoint, PullSubscriptionBatch, SeekOutcome, Subscription};

#[utoipa::path(
    post,
    path = "/topics/{topic}/subscriptions",
    params(("topic" = String, Path, description = "Topic name")),
    request_body = SubscriptionCreateRequest,
    responses(
        (status = 201, description = "Created subscription", body = Subscription),
        (status = 507, description = "Node is in ENOSPC degraded read-only mode (storage_full)")
    )
)]
pub async fn subscription_create(
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
    let req: SubscriptionCreateRequest = match parse_body(&body) {
        Ok(req) => req,
        Err(bad) => return bad.into_response(),
    };
    match service.create_subscription(topic, req.name).await {
        Ok(value) => (StatusCode::CREATED, Json(value)).into_response(),
        Err(error) => error.into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/topics/{topic}/subscriptions",
    params(("topic" = String, Path, description = "Topic name")),
    responses((status = 200, description = "Topic subscriptions", body = SubscriptionListResponse))
)]
pub async fn subscription_list(
    State(service): State<JournalService>,
    Extension(principal): Extension<AuditedRoleMapPrincipal>,
    Path(topic): Path<String>,
) -> Response {
    if let Err(deny) = authorize(&principal, &topic, Role::Read) {
        return deny.into_response();
    }
    let subscriptions = service.subscriptions(&topic);
    (
        StatusCode::OK,
        Json(SubscriptionListResponse { subscriptions }),
    )
        .into_response()
}

#[utoipa::path(
    get,
    path = "/topics/{topic}/subscriptions/{subscription}",
    params(
        ("topic" = String, Path, description = "Topic name"),
        ("subscription" = String, Path, description = "Subscription name")
    ),
    responses((status = 200, description = "Subscription", body = Subscription))
)]
pub async fn subscription_get(
    State(service): State<JournalService>,
    Extension(principal): Extension<AuditedRoleMapPrincipal>,
    Path((topic, name)): Path<(String, String)>,
) -> Response {
    if let Err(deny) = authorize(&principal, &topic, Role::Read) {
        return deny.into_response();
    }
    match service.subscription(&topic, &name) {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => error.into_response(),
    }
}

#[utoipa::path(
    delete,
    path = "/topics/{topic}/subscriptions/{subscription}",
    params(
        ("topic" = String, Path, description = "Topic name"),
        ("subscription" = String, Path, description = "Subscription name")
    ),
    responses(
        (status = 200, description = "Deleted subscription", body = Subscription),
        (status = 507, description = "Node is in ENOSPC degraded read-only mode (storage_full)")
    )
)]
pub async fn subscription_delete(
    State(service): State<JournalService>,
    Extension(principal): Extension<AuditedRoleMapPrincipal>,
    Path((topic, name)): Path<(String, String)>,
) -> Response {
    if let Err(deny) = authorize(&principal, &topic, Role::Write) {
        return deny.into_response();
    }
    // A delete shrinks the journal, so it is tempting to exempt it — but the
    // persist that follows still rewrites the WHOLE journal through a temp
    // file, which needs room for a second copy before the old one is unlinked.
    // On a full disk a delete fails exactly like an append.
    if let Err(full) = service.storage_writable() {
        return full.into_response();
    }
    match service.delete_subscription(topic, name).await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => error.into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/topics/{topic}/subscriptions/{subscription}/pull",
    params(
        ("topic" = String, Path, description = "Topic name"),
        ("subscription" = String, Path, description = "Subscription name")
    ),
    request_body = Option<SubscriptionPullRequest>,
    responses((status = 200, description = "Side-effect-free bounded replay window", body = PullSubscriptionBatch))
)]
pub async fn subscription_pull(
    State(service): State<JournalService>,
    Extension(principal): Extension<AuditedRoleMapPrincipal>,
    Path((topic, name)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> Response {
    if let Err(deny) = authorize(&principal, &topic, Role::Read) {
        return deny.into_response();
    }
    let req: SubscriptionPullRequest = if body.is_empty() {
        SubscriptionPullRequest { limit: None }
    } else {
        match parse_body(&body) {
            Ok(req) => req,
            Err(bad) => return bad.into_response(),
        }
    };
    match service.pull(&topic, &name, req.limit) {
        Ok(batch) => (StatusCode::OK, Json::<PullSubscriptionBatch>(batch)).into_response(),
        Err(error) => error.into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/topics/{topic}/subscriptions/{subscription}/ack",
    params(
        ("topic" = String, Path, description = "Topic name"),
        ("subscription" = String, Path, description = "Subscription name")
    ),
    request_body = SubscriptionAckRequest,
    responses(
        (status = 200, description = "Explicitly advanced checkpoint", body = ConsumerCheckpoint),
        (status = 409, description = "Stale or beyond-end offset, or expected_offset differs from the current cursor (conflict); stale epoch (stale_epoch)"),
        (status = 507, description = "Node is in ENOSPC degraded read-only mode (storage_full)")
    )
)]
pub async fn subscription_ack(
    State(service): State<JournalService>,
    Extension(principal): Extension<AuditedRoleMapPrincipal>,
    Path((topic, name)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> Response {
    if let Err(deny) = authorize(&principal, &topic, Role::Read) {
        return deny.into_response();
    }
    if let Err(full) = service.storage_writable() {
        return full.into_response();
    }
    let req: SubscriptionAckRequest = match parse_body(&body) {
        Ok(req) => req,
        Err(bad) => return bad.into_response(),
    };
    match service
        .ack_subscription(topic, name, req.offset, req.expected_offset, req.epoch)
        .await
    {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => error.into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/topics/{topic}/subscriptions/{subscription}/seek",
    params(
        ("topic" = String, Path, description = "Topic name"),
        ("subscription" = String, Path, description = "Subscription name")
    ),
    request_body = SubscriptionSeekRequest,
    responses(
        (status = 200, description = "Cursor rewound and epoch bumped", body = SeekOutcome),
        (status = 404, description = "Unknown subscription (subscription_error)"),
        (status = 409, description = "Offset outside the retained range (out_of_range)"),
        (status = 507, description = "Node is in ENOSPC degraded read-only mode (storage_full)")
    )
)]
pub async fn subscription_seek(
    State(service): State<JournalService>,
    Extension(principal): Extension<AuditedRoleMapPrincipal>,
    Path((topic, name)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> Response {
    if let Err(deny) = authorize(&principal, &topic, Role::Read) {
        return deny.into_response();
    }
    if let Err(full) = service.storage_writable() {
        return full.into_response();
    }
    let req: SubscriptionSeekRequest = match parse_body(&body) {
        Ok(req) => req,
        Err(bad) => return bad.into_response(),
    };
    match service.seek_subscription(topic, name, req.offset).await {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => error.into_response(),
    }
}
