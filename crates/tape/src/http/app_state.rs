//! [`AppState`]: one serving node's wiring — the journal service on its
//! durable backend, the bearer verifier, the optional raft group, and the
//! data-plane body limit.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use service_auth::ReloadableRoleMapVerifier;

use tape_access::AuthConfig;
use tape_journal::application::{JournalService, TapeMetrics};
use tape_replication::raft::TapeRaft;
use tape_shared_kernel::{TapeJournal, TapeOutcome};
use tape_storage::file_log::FileLog;
use tape_storage::wal::CommitCoordinator;

/// Shared application state: the [`JournalService`] every handler runs, the
/// bearer verifier the data-plane auth layer runs (#1326), the optional raft
/// group (#1327) that replicates mutations in HA (`REPLICAS_PER_SHARD > 1`)
/// mode, and the configured data-plane request body size limit (#2484).
/// `raft` stays `None` in single-node serving, where mutations commit through
/// the service's single-node log instead.
#[derive(Clone)]
pub struct AppState {
    service: JournalService,
    verifier: Arc<ReloadableRoleMapVerifier>,
    raft: Option<Arc<TapeRaft>>,
    body_limit_bytes: usize,
}

impl AppState {
    /// Build state from an already-loaded journal (empty when no `--store`
    /// file exists yet, mirroring the CLI's `load_journal`). Mutations commit
    /// through the legacy whole-file store at `store`, or nowhere durable when
    /// it is `None`. Auth is open (tokenless — the `TAPE_AUTH=off` default);
    /// production serving builds through [`AppState::with_auth`].
    pub fn new(journal: TapeJournal, store: Option<PathBuf>, body_limit_bytes: usize) -> Self {
        let journal = Arc::new(Mutex::new(journal));
        let log = Arc::new(FileLog::new(Arc::clone(&journal), store));
        Self {
            service: JournalService::new(journal, log),
            verifier: Arc::new(ReloadableRoleMapVerifier::open()),
            raft: None,
            body_limit_bytes,
        }
    }

    /// Build state with a resolved auth config (`--auth` /
    /// `--token-registry-file`): the data-plane auth layer runs the registry
    /// verifier when auth is required, the open verifier when off.
    pub fn with_auth(
        journal: TapeJournal,
        store: Option<PathBuf>,
        auth: AuthConfig,
        body_limit_bytes: usize,
    ) -> Self {
        let mut state = Self::new(journal, store, body_limit_bytes);
        state.verifier = Arc::new(auth.verifier());
        state
    }

    /// #3052: move this state onto the WAL group-commit path for `tape serve
    /// --data-dir`. The coordinator must apply into [`Self::journal_handle`].
    pub fn with_wal(mut self, coordinator: Arc<CommitCoordinator>) -> Self {
        self.service.set_log(coordinator);
        self
    }

    /// Attach the raft group (auto-mode HA serve path, #1327). Once set,
    /// every mutation proposes through it instead of the single-node log,
    /// and the router serves its peer routes.
    pub fn set_raft(&mut self, raft: Arc<TapeRaft>) {
        self.service.set_replicator(raft.clone());
        self.raft = Some(raft);
    }

    /// The journal use cases the data-plane handlers run.
    pub fn service(&self) -> &JournalService {
        &self.service
    }

    /// The bearer verifier the data-plane auth middleware runs.
    pub fn verifier(&self) -> Arc<ReloadableRoleMapVerifier> {
        Arc::clone(&self.verifier)
    }

    /// The per-op request metrics `/metrics` renders.
    pub fn metrics(&self) -> Arc<TapeMetrics> {
        self.service.metrics()
    }

    /// The shared journal handle, for wiring a raft group or a WAL
    /// coordinator onto the SAME journal this state serves reads from.
    pub fn journal_handle(&self) -> Arc<Mutex<TapeJournal>> {
        self.service.journal_handle()
    }

    /// The raft group this state proposes through, when running in HA mode.
    pub fn raft(&self) -> Option<Arc<TapeRaft>> {
        self.raft.clone()
    }

    /// The configured data-plane request body size limit (bytes).
    pub fn body_limit_bytes(&self) -> usize {
        self.body_limit_bytes
    }

    /// Flip readiness to draining so `/readyz` returns 503. Called on
    /// SIGTERM via `service_http::shutdown_with_drain`.
    pub fn start_drain(&self) {
        self.service.start_drain();
    }

    pub fn is_draining(&self) -> bool {
        self.service.is_draining()
    }

    /// Apply CR startup subscription provisioning on a single node; see
    /// [`JournalService::provision_startup_subscription`].
    pub async fn provision_startup_subscription(
        &self,
        topic: String,
        name: String,
    ) -> std::io::Result<TapeOutcome> {
        self.service
            .provision_startup_subscription(topic, name)
            .await
    }
}
