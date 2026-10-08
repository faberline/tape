//! CR-declared startup subscriptions (`TAPE_PROVISION_TOPICS`), ensured
//! idempotently before the listener starts.

use tape::http::AppState;
use tape_shared_kernel::{SubscriptionError, TapeOutcome};

/// Declarative topic/subscription provisioning from CR.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProvisionTopic {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) subscriptions: Vec<String>,
}

/// Ensure subscriptions from CR-declared topics. Called after journal/raft
/// is ready but before listener starts. Parses TAPE_PROVISION_TOPICS (compact JSON
/// array of {name, subscriptions}), ensures each subscription, and logs decisions
/// (created|already_exists|noted_implicit). Single-node provisioning uses the
/// durable mutation seam; HA keeps its established direct-memory path.
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

            // Attempt to create the subscription using the same path the API uses.
            // HA provisioning runs before the listener starts, so it keeps its
            // established in-process journal mutation path.
            let journal_handle = state.journal_handle();
            let mut journal = journal_handle.lock().expect("journal mutex poisoned");
            match journal.create_subscription(&topic.name, subscription) {
                Ok(_) => {
                    tracing::info!(
                        topic = %topic.name,
                        subscription = %subscription,
                        decision = "created",
                        "subscription created (cr-provisioned)"
                    );
                }
                Err(SubscriptionError::AlreadyExists { .. }) => {
                    tracing::info!(
                        topic = %topic.name,
                        subscription = %subscription,
                        decision = "already_exists",
                        "subscription already exists (idempotent)"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        topic = %topic.name,
                        subscription = %subscription,
                        error = %e,
                        "subscription ensure failed; continuing with other subscriptions"
                    );
                }
            }
        }
    }
}
