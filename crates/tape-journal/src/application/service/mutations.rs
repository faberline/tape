//! Mutations: each resolves its timestamps once, then proposes through the
//! raft group or commits through the single-node log.

use std::sync::atomic::Ordering;

use super::JournalService;
use crate::application::clock::now_ms;
use crate::application::error::JournalError;
use tape_shared_kernel::{
    should_enter_storage_degraded_mode, ConsumerCheckpoint, RetentionOutcome, RetentionPolicy,
    Subscription, SubscriptionError, TapeCommand, TapeEvent, TapeOutcome,
};

impl JournalService {
    /// #2573: sticky ENOSPC degraded read-only mode. Callers check this after
    /// authorizing and before parsing or applying a mutation — so a node that
    /// has already taken a genuine durability hit fast-fails each subsequent
    /// mutation with [`JournalError::StorageDegraded`] instead of re-running
    /// (and re-failing) the same write. A full disk then costs one status
    /// code per request, not one failed write.
    ///
    /// Ordering is deliberate: authorization runs FIRST. Node storage state is
    /// operational information, and an unauthenticated caller has no business
    /// learning it.
    ///
    /// Reads are exempt by construction (they never call this) and keep
    /// serving while degraded, which is the whole point of "degraded
    /// read-only" rather than "unready". `/readyz` stays green for the same
    /// reason. Deletes and retention are NOT exempt: shrinking the journal
    /// still rewrites it durably, which needs room on a full disk too.
    ///
    /// In replica/HA mode the gauge is never set: raft-runtime owns the
    /// durable write path (and any ENOSPC handling on it) instead.
    pub fn storage_writable(&self) -> Result<(), JournalError> {
        if self.metrics.is_storage_degraded() {
            return Err(JournalError::StorageDegraded);
        }
        Ok(())
    }

    /// Append one event. The event time (now when `timestamp_ms` is `None`)
    /// and `applied_at_ms` are both resolved HERE, before the command is
    /// proposed or committed, so every replica applies the identical values
    /// (#1327). #3052 D3: `applied_at_ms` is always stamped -- passing `0`
    /// would make `apply_command`'s fallback treat a CLIENT-supplied
    /// `timestamp_ms` as the applied time.
    ///
    /// Append is NOT idempotent (unlike a message_id-keyed publish), so an
    /// aged-out replicated outcome is surfaced as unavailable rather than
    /// silently re-appended.
    pub async fn append(
        &self,
        topic: String,
        key: Option<String>,
        payload: serde_json::Value,
        timestamp_ms: Option<u64>,
    ) -> Result<TapeEvent, JournalError> {
        let timestamp_ms = timestamp_ms.unwrap_or_else(now_ms);
        let command = TapeCommand::Append {
            topic,
            key,
            payload,
            timestamp_ms,
            applied_at_ms: now_ms(),
        };
        match self.mutate("append", command).await? {
            TapeOutcome::Appended(event) => Ok(event),
            _ => Err(outcome_mismatch("append")),
        }
    }

    pub async fn put_checkpoint(
        &self,
        topic: String,
        consumer: String,
        offset: u64,
    ) -> Result<ConsumerCheckpoint, JournalError> {
        let command = TapeCommand::CheckpointPut {
            topic,
            consumer,
            offset,
            updated_at_ms: now_ms(),
        };
        match self.mutate("checkpoint_put", command).await? {
            TapeOutcome::Checkpoint(result) => result.map_err(JournalError::from),
            _ => Err(outcome_mismatch("checkpoint_put")),
        }
    }

    pub async fn create_subscription(
        &self,
        topic: String,
        name: String,
    ) -> Result<Subscription, JournalError> {
        let command = TapeCommand::SubscriptionCreate { topic, name };
        match self.mutate("subscription_create", command).await? {
            TapeOutcome::SubscriptionCreated(result) => result.map_err(JournalError::from),
            _ => Err(outcome_mismatch("subscription_create")),
        }
    }

    pub async fn delete_subscription(
        &self,
        topic: String,
        name: String,
    ) -> Result<Subscription, JournalError> {
        let command = TapeCommand::SubscriptionDelete { topic, name };
        match self.mutate("subscription_delete", command).await? {
            TapeOutcome::SubscriptionDeleted(result) => result.map_err(JournalError::from),
            _ => Err(outcome_mismatch("subscription_delete")),
        }
    }

    pub async fn ack_subscription(
        &self,
        topic: String,
        name: String,
        offset: u64,
    ) -> Result<ConsumerCheckpoint, JournalError> {
        let command = TapeCommand::SubscriptionAck {
            topic,
            name,
            offset,
            updated_at_ms: now_ms(),
        };
        match self.mutate("subscription_ack", command).await? {
            TapeOutcome::SubscriptionAcked(result) => result.map_err(JournalError::from),
            _ => Err(outcome_mismatch("subscription_ack")),
        }
    }

    pub async fn put_retention(
        &self,
        topic: String,
        policy: RetentionPolicy,
    ) -> Result<RetentionOutcome, JournalError> {
        let command = TapeCommand::RetentionPut {
            topic,
            policy,
            now_ms: now_ms(),
        };
        match self.mutate("retention_put", command).await? {
            TapeOutcome::RetentionUpdated(outcome) => Ok(outcome),
            _ => Err(outcome_mismatch("retention_put")),
        }
    }

    /// Propose through the raft group when one is attached, otherwise commit
    /// through the single-node log.
    async fn mutate(
        &self,
        operation: &str,
        command: TapeCommand,
    ) -> Result<TapeOutcome, JournalError> {
        let Some(replicator) = &self.replicator else {
            return self
                .apply_mutation(command)
                .await
                .map_err(JournalError::Durability);
        };
        match replicator.propose(command).await {
            Ok(Some(outcome)) => Ok(outcome),
            Ok(None) => Err(JournalError::Unavailable(match operation {
                "append" => "append outcome aged out before this node could read it back".into(),
                "checkpoint_put" => {
                    "checkpoint outcome aged out before this node could read it back".into()
                }
                _ => format!("{operation} outcome unavailable after commit"),
            })),
            Err(error) => Err(JournalError::Unavailable(error.0)),
        }
    }

    /// Apply one mutating [`TapeCommand`] through the single-node
    /// [`CommitLog`] -- WAL group commit, the legacy whole-file store, or no
    /// local store at all -- and report the same [`TapeOutcome`] vocabulary
    /// the raft path produces, so every serving path shares one mutation
    /// semantics.
    ///
    /// A domain-level rejection -- e.g. `TapeOutcome::Checkpoint(Err(TapeError
    /// ::StaleCheckpoint))` -- is NOT a durability failure and is reported as
    /// `Ok(TapeOutcome::Checkpoint(Err(..)))`, never this function's own
    /// `Err`. The command is applied (and, in WAL mode, already durably
    /// written) exactly as if it had succeeded -- mirroring the raft path,
    /// whose state machine always applies regardless of the wrapped
    /// `TapeOutcome`. Do not "validate before writing" to route around this
    /// asymmetry; that would silently re-diverge the single-node and
    /// raft-replicated apply semantics #3052 unified. This function's own
    /// `Err` is reserved for durability failures only.
    ///
    /// WI #3052 R6: an ENOSPC or EIO durability failure latches sticky
    /// degraded mode (see
    /// [`should_enter_storage_degraded_mode`]); any other failure fails this
    /// one request closed without flipping the node read-only.
    pub async fn apply_mutation(&self, command: TapeCommand) -> std::io::Result<TapeOutcome> {
        // #2573 test seam (#3052: fires identically ahead of EVERY backend,
        // including WAL mode). See `JournalService::set_inject_storage_full`.
        if self.inject_storage_full.load(Ordering::SeqCst) {
            self.metrics.mark_storage_degraded();
            return Err(std::io::Error::from(std::io::ErrorKind::StorageFull));
        }

        let result = self.log.commit(command).await;
        if let Err(error) = &result {
            if should_enter_storage_degraded_mode(error) {
                self.metrics.mark_storage_degraded();
            }
        }
        result
    }

    /// Apply CR startup subscription provisioning on a single node.
    ///
    /// Recovered subscriptions do not need another command. A missing one
    /// must use the normal single-node mutation path so WAL recovery sees it
    /// on the next boot. Callers keep HA provisioning on its established path.
    pub async fn provision_startup_subscription(
        &self,
        topic: String,
        name: String,
    ) -> std::io::Result<TapeOutcome> {
        assert!(
            self.replicator.is_none(),
            "startup subscription seam is single-node only"
        );
        let exists = self.lock().subscription(&topic, &name).is_some();
        if exists {
            return Ok(TapeOutcome::SubscriptionCreated(Err(
                SubscriptionError::AlreadyExists { topic, name },
            )));
        }
        self.apply_mutation(TapeCommand::SubscriptionCreate { topic, name })
            .await
    }
}

fn outcome_mismatch(operation: &str) -> JournalError {
    JournalError::Internal(format!("raft outcome kind mismatch for {operation}"))
}
