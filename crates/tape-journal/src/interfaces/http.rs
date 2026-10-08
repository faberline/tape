//! axum HTTP interface over the journal.
//!
//! Handlers are thin: each authorizes the caller on its `{topic}` through
//! [`tape_access::authorize`], parses the request, and calls
//! one [`JournalService`] use case — no domain behavior lives here. Error
//! responses render the shared `{error, message}` envelope
//! ([`service_http::ApiErr`]).
//!
//! Request auth is the shared `service-auth` bearer contract (#1326): the
//! blanket `service_auth::auth_middleware` (layered by the assembly in
//! `tape::http`) runs on the data plane ONLY, injecting the
//! [`service_auth::AuditedRoleMapPrincipal`] each handler authorizes with —
//! `append`/subscription create+delete/retention put = write, everything a
//! consumer does (including advancing its own checkpoint) = read, and
//! `/admin/backup` = admin on `*`.

use axum::routing::{get, post};
use axum::Router;

use crate::application::JournalService;

mod admin;
mod dto;
mod errors;
mod openapi;
mod probes;
mod retention;
mod subscriptions;
mod topics;
mod track;

pub use dto::{
    AppendRequest, CheckpointPutRequest, CheckpointResponse, ReplayQuery, ReplayResponse,
    RetentionGetResponse, SubscriptionAckRequest, SubscriptionCreateRequest,
    SubscriptionListResponse, SubscriptionPullRequest,
};
pub use openapi::{openapi, ApiDoc};
pub use track::track;

/// The `/topics` data plane and `/admin/backup`, unlayered: the assembly adds
/// auth, request metrics, the body limit, and admission around it.
pub fn data_plane_routes() -> Router<JournalService> {
    Router::new()
        .route("/topics/{topic}/append", post(topics::append))
        .route("/topics/{topic}/replay", get(topics::replay))
        .route("/topics/{topic}/replay/stream", get(topics::replay_stream))
        .route(
            "/topics/{topic}/consumers/{consumer}/checkpoint",
            get(topics::checkpoint_get).put(topics::checkpoint_put),
        )
        .route(
            "/topics/{topic}/subscriptions",
            get(subscriptions::subscription_list).post(subscriptions::subscription_create),
        )
        .route(
            "/topics/{topic}/subscriptions/{subscription}",
            get(subscriptions::subscription_get).delete(subscriptions::subscription_delete),
        )
        .route(
            "/topics/{topic}/subscriptions/{subscription}/pull",
            post(subscriptions::subscription_pull),
        )
        .route(
            "/topics/{topic}/subscriptions/{subscription}/ack",
            post(subscriptions::subscription_ack),
        )
        .route(
            "/topics/{topic}/retention",
            get(retention::retention_get).put(retention::retention_put),
        )
        // Cluster-wide admin op (#1329): a consistent snapshot of the journal
        // for backup runners. Inside the auth layer (unlike probes) — needs
        // `admin` on `*`.
        .route("/admin/backup", get(admin::admin_backup))
}
