//! utoipa OpenAPI document for tape's HTTP transport.
//!
//! The path operations are declared by `#[utoipa::path]` on the handlers in
//! this module's siblings; this module collects them into one document and
//! renders it as JSON for the `/openapi.json` endpoint. Independent of the
//! hand-rolled JSON contract `tape spec` prints.

use utoipa::OpenApi;

/// The served OpenAPI document.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "tape HTTP transport",
        description = "Topic append/replay/checkpoint journal over HTTP/1.1 + h2c."
    ),
    paths(
        super::topics::append,
        super::topics::replay,
        super::topics::replay_stream,
        super::topics::checkpoint_get,
        super::topics::checkpoint_put,
        super::subscriptions::subscription_create,
        super::subscriptions::subscription_list,
        super::subscriptions::subscription_get,
        super::subscriptions::subscription_delete,
        super::subscriptions::subscription_pull,
        super::subscriptions::subscription_ack,
        super::retention::retention_get,
        super::retention::retention_put,
        super::admin::admin_backup,
    ),
    components(schemas(
        tape_shared_kernel::TapeEvent,
        tape_shared_kernel::ConsumerCheckpoint,
        super::dto::AppendRequest,
        super::dto::ReplayResponse,
        super::dto::CheckpointResponse,
        super::dto::CheckpointPutRequest,
        tape_shared_kernel::Subscription,
        tape_shared_kernel::PullSubscriptionBatch,
        super::dto::SubscriptionCreateRequest,
        super::dto::SubscriptionListResponse,
        super::dto::SubscriptionPullRequest,
        super::dto::SubscriptionAckRequest,
        tape_shared_kernel::RetentionPolicy,
        tape_shared_kernel::RetentionOutcome,
        super::dto::RetentionGetResponse,
    ))
)]
pub struct ApiDoc;

/// The tape OpenAPI document — the accessor the shared `service_http`
/// `/openapi.json` and `/docs` probe routes serve (a
/// `fn() -> utoipa::openapi::OpenApi` pointer).
pub fn openapi() -> utoipa::openapi::OpenApi {
    ApiDoc::openapi()
}

#[cfg(test)]
mod tests {
    use super::openapi;

    #[test]
    fn lists_the_public_endpoints() {
        let doc = openapi().to_pretty_json().unwrap();
        for path in [
            "/topics/{topic}/append",
            "/topics/{topic}/replay",
            "/topics/{topic}/replay/stream",
            "/topics/{topic}/consumers/{consumer}/checkpoint",
            "/topics/{topic}/subscriptions",
            "/topics/{topic}/subscriptions/{subscription}",
            "/topics/{topic}/subscriptions/{subscription}/pull",
            "/topics/{topic}/subscriptions/{subscription}/ack",
            "/topics/{topic}/retention",
            "/admin/backup",
        ] {
            assert!(doc.contains(path), "OpenAPI doc must list {path}");
        }
    }
}
