//! CR-declared startup subscriptions (`TAPE_PROVISION_TOPICS`), ensured
//! idempotently.
//!
//! Single-node provisioning ensures subscriptions through the durable mutation
//! seam before the listener binds. HA mode provisions through the replicated
//! journal mutation path (`JournalService::create_subscription`), which proposes
//! through Raft and forwards to the leader, retrying while no leader is elected.

use std::time::Duration;

use tape::http::AppState;
use tape_journal::application::JournalError;
use tape_shared_kernel::{Subscription, SubscriptionError, TapeOutcome};

#[cfg(test)]
mod tests;

/// Declarative topic/subscription provisioning from CR.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProvisionTopic {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) subscriptions: Vec<String>,
}

/// Classification of a subscription create attempt in HA mode.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ProvisionDecision {
    /// Subscription was created successfully.
    Created,
    /// Subscription already exists (idempotent no-op).
    AlreadyExists,
    /// Proposal failed due to election / not-yet-leader / unavailable; retry.
    Retry(String),
    /// Non-retryable error.
    Failed(String),
}

/// Classify the result of [`AppState::create_subscription`] in HA mode to
/// determine whether to retry, log success, or log failure.
pub(crate) fn classify_ha_create_result(
    result: &Result<Subscription, JournalError>,
) -> ProvisionDecision {
    match result {
        Ok(_) => ProvisionDecision::Created,
        Err(JournalError::SubscriptionExists(_)) => ProvisionDecision::AlreadyExists,
        Err(JournalError::Unavailable(reason)) => ProvisionDecision::Retry(reason.clone()),
        Err(error) => ProvisionDecision::Failed(error.to_string()),
    }
}

/// Ensure subscriptions from CR-declared topics.
///
/// Parses `TAPE_PROVISION_TOPICS` (compact JSON array of `{name, subscriptions}`),
/// ensures each subscription, and logs decisions (`created` | `already_exists` |
/// `noted_implicit`).
///
/// In single-node mode, this uses [`AppState::provision_startup_subscription`].
/// In HA mode, every subscription is proposed through Raft via
/// [`AppState::create_subscription`], retrying with bounded backoff while
/// no leader exists.
pub(crate) async fn ensure_subscriptions(state: &AppState) {
    let json_str = match std::env::var("TAPE_PROVISION_TOPICS") {
        Ok(s) => s,
        Err(std::env::VarError::NotPresent) => return, // No provisioning declared
        Err(e) => {
            tracing::warn!(error = %e, "failed to read TAPE_PROVISION_TOPICS env");
            return;
        }
    };

    let topics: Vec<ProvisionTopic> = match serde_json::from_str(&json_str) {
        Ok(topics) => topics,
        Err(e) => {
            tracing::warn!(error = %e, json = %json_str, "failed to parse TAPE_PROVISION_TOPICS");
            return;
        }
    };

    for topic in topics {
        if topic.subscriptions.is_empty() {
            // Topic declared with no subscriptions: no journal mutation, just log the declaration
            tracing::info!(
                topic = %topic.name,
                decision = "noted_implicit",
                "topic provisioning noted (no subscriptions declared)"
            );
            continue;
        }

        for subscription in &topic.subscriptions {
            if state.raft().is_none() {
                match state
                    .provision_startup_subscription(topic.name.clone(), subscription.clone())
                    .await
                {
                    Ok(TapeOutcome::SubscriptionCreated(Ok(_))) => {
                        tracing::info!(
                            topic = %topic.name,
                            subscription = %subscription,
                            decision = "created",
                            "subscription created (cr-provisioned)"
                        );
                    }
                    Ok(TapeOutcome::SubscriptionCreated(Err(
                        SubscriptionError::AlreadyExists { .. },
                    ))) => {
                        tracing::info!(
                            topic = %topic.name,
                            subscription = %subscription,
                            decision = "already_exists",
                            "subscription already exists (idempotent)"
                        );
                    }
                    Ok(outcome) => {
                        tracing::warn!(
                            topic = %topic.name,
                            subscription = %subscription,
                            ?outcome,
                            "subscription ensure returned an unexpected outcome; continuing with other subscriptions"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            topic = %topic.name,
                            subscription = %subscription,
                            %error,
                            "subscription ensure failed; continuing with other subscriptions"
                        );
                    }
                }
                continue;
            }

            // In HA mode, propose subscription creation through Raft, retrying with
            // bounded backoff while the group has no leader or is electing.
            let mut backoff = Duration::from_millis(50);
            let max_backoff = Duration::from_millis(500);
            loop {
                if state.is_draining() {
                    tracing::debug!(
                        topic = %topic.name,
                        subscription = %subscription,
                        "node draining; stopping provisioning attempts"
                    );
                    break;
                }

                let result = state
                    .create_subscription(topic.name.clone(), subscription.clone())
                    .await;

                match classify_ha_create_result(&result) {
                    ProvisionDecision::Created => {
                        tracing::info!(
                            topic = %topic.name,
                            subscription = %subscription,
                            decision = "created",
                            "subscription created (cr-provisioned)"
                        );
                        break;
                    }
                    ProvisionDecision::AlreadyExists => {
                        tracing::info!(
                            topic = %topic.name,
                            subscription = %subscription,
                            decision = "already_exists",
                            "subscription already exists (idempotent)"
                        );
                        break;
                    }
                    ProvisionDecision::Retry(reason) => {
                        tracing::debug!(
                            topic = %topic.name,
                            subscription = %subscription,
                            %reason,
                            "provisioning proposal unavailable; retrying with backoff"
                        );
                        tokio::time::sleep(backoff).await;
                        backoff = std::cmp::min(backoff.saturating_mul(2), max_backoff);
                    }
                    ProvisionDecision::Failed(error) => {
                        tracing::warn!(
                            topic = %topic.name,
                            subscription = %subscription,
                            %error,
                            "subscription ensure failed; continuing with other subscriptions"
                        );
                        break;
                    }
                }
            }
        }
    }
}
