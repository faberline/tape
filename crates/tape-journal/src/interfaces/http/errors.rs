//! How a failed use case renders: the shared `{error, message}` envelope with
//! tape's status and machine-readable code per failure.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;
use service_http::ApiErr;

use crate::application::JournalError;

impl IntoResponse for JournalError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            // #2573: the request that FIRST hits a full disk and every
            // request after it (fast-failed while degraded) share one
            // envelope, so a client cannot tell them apart.
            JournalError::StorageDegraded => (StatusCode::INSUFFICIENT_STORAGE, "storage_full"),
            JournalError::Durability(error) if error.kind() == std::io::ErrorKind::StorageFull => {
                return ApiErr::new(
                    StatusCode::INSUFFICIENT_STORAGE,
                    "storage_full",
                    format!("journal persist failed: local storage is full (ENOSPC): {error}"),
                )
                .into_response();
            }
            JournalError::Durability(_) | JournalError::Internal(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "internal")
            }
            JournalError::Unavailable(_) => (StatusCode::SERVICE_UNAVAILABLE, "raft_unavailable"),
            JournalError::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            JournalError::SubscriptionNotFound(_) => (StatusCode::NOT_FOUND, "subscription_error"),
            JournalError::SubscriptionExists(_) => (StatusCode::CONFLICT, "subscription_error"),
            JournalError::PullBatchTooLarge(_) => (StatusCode::BAD_REQUEST, "subscription_error"),
        };
        ApiErr::new(status, code, self.to_string()).into_response()
    }
}

/// Decode a JSON request body, rendering a malformed one as `400
/// bad_request`.
pub(super) fn parse_body<T: DeserializeOwned>(body: &[u8]) -> Result<T, ApiErr> {
    serde_json::from_slice(body)
        .map_err(|error| ApiErr::new(StatusCode::BAD_REQUEST, "bad_request", error.to_string()))
}
