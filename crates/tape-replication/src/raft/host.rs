//! One running raft group over the journal, and its [`Replicator`] seam.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use axum::Router;
use raft_runtime::{
    ClusterTopology, FsyncPolicy, HostConfig, HostShutdownReport, Index, Membership, NodeId,
    PeerTransport, RaftHost, RaftStateMachine, RaftStore, SnapshotPolicy,
};
use server_lifecycle::ShutdownDeadline;

use super::{TapeEnvelope, TapeStateMachine};
use tape_shared_kernel::{
    BoxFuture, ProposalId, ReplicationError, Replicator, RetentionPolicy, TapeCommand, TapeJournal,
    TapeOutcome,
};

/// One running raft group over a tape journal -- the single-group wrapper the
/// serve path (and tests) hold. Dropping it aborts the host's tick/pump tasks.
pub struct TapeRaft {
    host: RaftHost,
    sm: Arc<TapeStateMachine>,
    node_id: NodeId,
    session: u64,
    proposal_sequence: AtomicU64,
}

impl TapeRaft {
    /// Spawn the group for node `node_id`, persisting shared Raft hard state,
    /// commit watermark, log, and snapshots under `raft_dir`. `peers` maps the
    /// other members to base URLs (empty => single-node).
    pub fn spawn(
        journal: Arc<Mutex<TapeJournal>>,
        raft_dir: &Path,
        node_id: NodeId,
        membership: Membership,
        peers: HashMap<NodeId, String>,
        cfg: HostConfig,
    ) -> Result<TapeRaft> {
        Self::spawn_inner(journal, raft_dir, node_id, membership, peers, cfg, None)
    }

    /// Spawn a group whose outgoing peer RPCs and incoming Raft listener use
    /// the shared mutually authenticated transport. The caller serves
    /// [`Self::router`] through the same transport on its dedicated port.
    pub fn spawn_with_peer_transport(
        journal: Arc<Mutex<TapeJournal>>,
        raft_dir: &Path,
        node_id: NodeId,
        membership: Membership,
        peers: HashMap<NodeId, String>,
        cfg: HostConfig,
        peer_transport: PeerTransport,
    ) -> Result<TapeRaft> {
        Self::spawn_inner(
            journal,
            raft_dir,
            node_id,
            membership,
            peers,
            cfg,
            Some(peer_transport),
        )
    }

    fn spawn_inner(
        journal: Arc<Mutex<TapeJournal>>,
        raft_dir: &Path,
        node_id: NodeId,
        membership: Membership,
        peers: HashMap<NodeId, String>,
        cfg: HostConfig,
        peer_transport: Option<PeerTransport>,
    ) -> Result<TapeRaft> {
        std::fs::create_dir_all(raft_dir)?;
        let dir = raft_dir
            .to_str()
            .context("raft data dir is not valid UTF-8")?;
        let store = RaftStore::open(dir, node_id, FsyncPolicy::Always)?;
        let sm = TapeStateMachine::new(
            journal,
            Some(raft_dir.join(format!("applied-{node_id}.idx"))),
        )?;
        let host = match peer_transport {
            Some(transport) => RaftHost::spawn_with_peer_transport(
                node_id,
                membership,
                peers,
                store,
                Arc::clone(&sm) as Arc<dyn RaftStateMachine>,
                cfg,
                transport,
            ),
            None => RaftHost::spawn(
                node_id,
                membership,
                peers,
                store,
                Arc::clone(&sm) as Arc<dyn RaftStateMachine>,
                cfg,
            ),
        };
        static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);
        let wall = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        let session = wall
            ^ ((std::process::id() as u64) << 32)
            ^ NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
        Ok(TapeRaft {
            host,
            sm,
            node_id,
            session,
            proposal_sequence: AtomicU64::new(1),
        })
    }

    /// Spawn from a k8s-derived [`ClusterTopology`] (the auto-mode serve
    /// path); raft state lives under `{data_dir}/raft`.
    pub fn from_topology(
        journal: Arc<Mutex<TapeJournal>>,
        data_dir: &Path,
        topo: &ClusterTopology,
        cfg: HostConfig,
    ) -> Result<TapeRaft> {
        Self::spawn(
            journal,
            &data_dir.join("raft"),
            topo.node_id,
            topo.membership.clone(),
            topo.peers.clone(),
            cfg,
        )
    }

    /// TLS-aware topology constructor. The topology must carry `https` peer
    /// URLs for the same dedicated listener the caller serves below.
    pub fn from_topology_with_peer_transport(
        journal: Arc<Mutex<TapeJournal>>,
        data_dir: &Path,
        topo: &ClusterTopology,
        cfg: HostConfig,
        peer_transport: PeerTransport,
    ) -> Result<TapeRaft> {
        Self::spawn_with_peer_transport(
            journal,
            &data_dir.join("raft"),
            topo.node_id,
            topo.membership.clone(),
            topo.peers.clone(),
            cfg,
            peer_transport,
        )
    }

    /// The standard host tuning for tape: default timing + compaction every
    /// `snapshot_every` applied entries (tests pass a small threshold to arm
    /// InstallSnapshot quickly).
    pub fn host_config(snapshot_every: u64) -> HostConfig {
        HostConfig {
            snapshot: SnapshotPolicy::EveryEntries(snapshot_every),
            ..HostConfig::default()
        }
    }

    /// Drain the shared Raft host's in-flight peer RPCs before its h2 client
    /// and peer listener are torn down.
    pub async fn shutdown(&self) -> Result<()> {
        self.host.shutdown().await
    }

    /// Refuse new proposals; in-flight ones still commit. True only for the
    /// first call. Public ingress should already be draining.
    pub fn quiesce_proposals(&self) -> bool {
        self.host.quiesce_proposals()
    }

    /// Shut the host down within `deadline`: quiesce, hand leadership to a
    /// caught-up voter, stop the background tasks and drain peer RPCs. The
    /// peer listener may close gracefully only when the report says
    /// `peer_listener_close_safe`; a repeat call joins the first one.
    pub async fn shutdown_within(&self, deadline: ShutdownDeadline) -> HostShutdownReport {
        self.host.shutdown_within(deadline).await
    }

    /// Peer raft RPCs + leader forwarding + `/raftz`. The h2c compatibility
    /// path merges this onto the public app; mTLS serves it independently.
    pub fn router(&self) -> Router {
        self.host.router()
    }

    /// Propose an append (leader-local or forwarded to the leader by the
    /// host) and claim the appended event once THIS node applied it
    /// (read-your-write). `None` means the outcome aged out of the window.
    pub async fn propose_append(
        &self,
        topic: String,
        key: Option<String>,
        payload: serde_json::Value,
        timestamp_ms: u64,
    ) -> Result<(Index, Option<TapeOutcome>)> {
        let cmd = TapeCommand::Append {
            topic,
            key,
            payload,
            timestamp_ms,
            applied_at_ms: now_ms(),
        };
        self.propose_and_claim(cmd).await
    }

    /// Propose a checkpoint-put and claim the outcome once THIS node applied
    /// it.
    pub async fn propose_checkpoint(
        &self,
        topic: String,
        consumer: String,
        offset: u64,
        updated_at_ms: u64,
    ) -> Result<(Index, Option<TapeOutcome>)> {
        let cmd = TapeCommand::CheckpointPut {
            topic,
            consumer,
            offset,
            updated_at_ms,
        };
        self.propose_and_claim(cmd).await
    }

    pub async fn propose_subscription_create(
        &self,
        topic: String,
        name: String,
    ) -> Result<(Index, Option<TapeOutcome>)> {
        self.propose_and_claim(TapeCommand::SubscriptionCreate { topic, name })
            .await
    }

    pub async fn propose_subscription_delete(
        &self,
        topic: String,
        name: String,
    ) -> Result<(Index, Option<TapeOutcome>)> {
        self.propose_and_claim(TapeCommand::SubscriptionDelete { topic, name })
            .await
    }

    pub async fn propose_subscription_ack(
        &self,
        topic: String,
        name: String,
        offset: u64,
        updated_at_ms: u64,
    ) -> Result<(Index, Option<TapeOutcome>)> {
        self.propose_and_claim(TapeCommand::SubscriptionAck {
            topic,
            name,
            offset,
            updated_at_ms,
        })
        .await
    }

    pub async fn propose_retention(
        &self,
        topic: String,
        policy: RetentionPolicy,
        now_ms: u64,
    ) -> Result<(Index, Option<TapeOutcome>)> {
        self.propose_and_claim(TapeCommand::RetentionPut {
            topic,
            policy,
            now_ms,
        })
        .await
    }

    /// Propose `command` and claim the outcome once THIS node applied it
    /// (read-your-write): first by its stable proposal id, which survives
    /// ambiguous retries, then by index. `None` means it aged out.
    async fn propose_and_claim(
        &self,
        command: TapeCommand,
    ) -> Result<(Index, Option<TapeOutcome>)> {
        let (index, proposal_id) = self.propose(command).await?;
        let outcome = self
            .sm
            .proposal_outcome(&proposal_id)
            .or_else(|| self.sm.claim_outcome(index));
        Ok((index, outcome))
    }

    async fn propose(&self, command: TapeCommand) -> Result<(Index, ProposalId)> {
        let envelope = TapeEnvelope {
            proposal_id: ProposalId {
                node: self.node_id,
                session: self.session,
                sequence: self.proposal_sequence.fetch_add(1, Ordering::Relaxed),
            },
            command,
        };
        let proposal_id = envelope.proposal_id.clone();
        let index = self.host.propose(serde_json::to_vec(&envelope)?).await?;
        Ok((index, proposal_id))
    }

    pub async fn is_leader(&self) -> bool {
        self.host.is_leader().await
    }

    pub async fn leader(&self) -> Option<NodeId> {
        self.host.leader().await
    }

    /// Highest raft index this node's journal has applied.
    pub fn applied_index(&self) -> Index {
        self.sm.applied_index()
    }

    /// Capture the exact state-machine snapshot, including the bounded
    /// proposal-outcome cache needed for ambiguous retry idempotency.
    pub fn snapshot_bytes(&self) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        self.sm.snapshot(&mut buf)?;
        Ok(buf)
    }

    /// The journal this group replicates into.
    pub fn journal(&self) -> Arc<Mutex<TapeJournal>> {
        self.sm.journal()
    }
}

impl Replicator for TapeRaft {
    fn propose(
        &self,
        command: TapeCommand,
    ) -> BoxFuture<'_, Result<Option<TapeOutcome>, ReplicationError>> {
        Box::pin(async move {
            self.propose_and_claim(command)
                .await
                .map(|(_, outcome)| outcome)
                .map_err(|error| ReplicationError(error.to_string()))
        })
    }

    fn applied_index(&self) -> u64 {
        TapeRaft::applied_index(self)
    }

    fn snapshot_bytes(&self) -> Result<Vec<u8>, ReplicationError> {
        TapeRaft::snapshot_bytes(self).map_err(|error| ReplicationError(error.to_string()))
    }
}

/// Wall-clock milliseconds for the `applied_at_ms` a direct
/// [`TapeRaft::propose_append`] stamps.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}
