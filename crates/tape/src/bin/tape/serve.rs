//! `tape serve`: resolve auth, the durable backend, and (in replica mode) the
//! raft group, then run the HTTP server until the shutdown sequence in
//! [`shutdown`] completes.

mod provision;
mod reprobe;
mod shutdown;
mod store;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::ValueEnum;
use tape::http::{router_with_admission, router_without_raft_routes_with_admission, AppState};
use tape_access as auth;
use tape_replication::{peer_tls, raft};
use tape_shared_kernel::TapeJournal;
use tape_storage::wal;

use crate::offline::load_journal;
use provision::ensure_subscriptions;
use reprobe::spawn_storage_full_reprobe;
use shutdown::{shutdown_on_signal, PublicHttp, ServerTask};
pub(crate) use store::{resolve_journal_store, JournalStoreKind};

#[derive(clap::Args, Debug)]
pub(crate) struct ServeArgs {
    /// h2c + HTTP/1.1 listen address.
    #[arg(long, env = "TAPE_BIND", default_value = "127.0.0.1:7137")]
    pub(crate) bind: String,
    /// Journal file to load at boot and persist to on every mutation.
    /// Defaults to an empty in-memory journal when unset.
    #[arg(long, env = "TAPE_STORE")]
    pub(crate) store: Option<PathBuf>,
    /// Total shutdown budget (seconds) after SIGTERM: the readiness drain,
    /// the raft leadership handoff and the listener drains all finish
    /// within it. Set the pod's `terminationGracePeriodSeconds` a few
    /// seconds above it.
    #[arg(long, env = "TAPE_GRACE_SECS", default_value_t = 10)]
    pub(crate) grace_secs: u64,
    /// Seconds after SIGTERM that the node keeps accepting requests while
    /// `/readyz` reports 503, so endpoints move off it before writes stop.
    /// Capped by `--grace-secs`.
    #[arg(long, env = "TAPE_DRAIN_DELAY_SECS", default_value_t = 5)]
    pub(crate) drain_delay_secs: u64,
    /// Log output format. Kubernetes uses `json` for the shared
    /// `axiom.service.log.v1` collector contract; local development defaults
    /// to the human-readable formatter.
    #[arg(long, env = "TAPE_LOG_FORMAT", value_enum, default_value_t = LogFormat::Pretty)]
    pub(crate) log_format: LogFormat,
    /// OTLP gRPC endpoint for opt-in shared trace export. Unset preserves
    /// logging-only startup; requires the `otel` build feature to export.
    #[arg(long, env = "TAPE_OTLP_ENDPOINT")]
    pub(crate) otlp_endpoint: Option<String>,
    /// Request-auth mode for the /topics data plane: `off` (tokenless dev,
    /// the default) or `required` (bearer tokens from the registry file).
    /// Probes stay tokenless either way.
    #[arg(long, env = "TAPE_AUTH", default_value = "off")]
    pub(crate) auth: String,
    /// Bearer-token registry file (JSON `{token: {subject, roles}}`),
    /// mounted from a Secret in production. Required (and validated at
    /// startup) when `--auth required`.
    #[arg(long, env = "TAPE_TOKEN_REGISTRY_FILE")]
    pub(crate) token_registry_file: Option<PathBuf>,
    /// Durable directory for shared Raft hard state, committed log, and
    /// snapshots (#1327). Required in replica/HA mode (`REPLICAS_PER_SHARD > 1`);
    /// in single-node mode it selects `journal.json` unless `--store` is
    /// supplied explicitly.
    #[arg(long, env = "TAPE_DATA_DIR")]
    pub(crate) data_dir: Option<PathBuf>,
    /// Exact `file://`, `s3://`, or `gs://` journal snapshot consulted to
    /// seed an EMPTY replica PVC before Raft starts (`gs://` authenticates
    /// via workload-identity ADC in-cluster). Bootstrap-if-empty (#2468): a
    /// non-empty `TAPE_DATA_DIR` means this pod already has durable state
    /// (including a routine restart onto its own PVC), so the seed fetch is
    /// skipped rather than refused — the field may harmlessly stay set on
    /// the CR after a successful bootstrap. This is cold recovery, not a
    /// live restore endpoint.
    #[arg(long, env = "TAPE_BOOTSTRAP_SEED_URI")]
    pub(crate) bootstrap_seed_uri: Option<String>,
    /// Headless service name peers are resolved against in replica/HA mode
    /// (`ClusterTopology::from_env`).
    #[arg(long, env = "TAPE_PEER_SERVICE", default_value = "tape")]
    pub(crate) peer_service: String,
    /// Dedicated port for authenticated Raft peer RPCs when
    /// `TAPE_PEER_MTLS=on`. The public h2c port remains unchanged.
    #[arg(long, env = "TAPE_RAFT_PORT", default_value_t = 7138)]
    pub(crate) raft_port: u16,
    /// Data-plane request body size limit (bytes). Requests with
    /// `Content-Length` exceeding this are rejected with 413; streamed bodies
    /// are bounded mid-read. Defaults to 8 MiB (`TAPE_BODY_LIMIT_BYTES`).
    #[arg(long, env = "TAPE_BODY_LIMIT_BYTES", default_value_t = 8 * 1024 * 1024)]
    pub(crate) body_limit_bytes: usize,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum LogFormat {
    Pretty,
    Json,
}

/// Run the tape HTTP server: load the journal from `--store` (or start
/// empty), serve the shared service shell (standard probes merged with the
/// `/topics` data plane) over HTTP/1.1 + h2c on one port, until SIGTERM
/// runs the shutdown sequence within `--grace-secs`.
pub(crate) async fn serve_main(args: ServeArgs) -> Result<()> {
    let log_format = match args.log_format {
        LogFormat::Pretty => service_http::LogFormat::Pretty,
        LogFormat::Json => service_http::LogFormat::Json,
    };
    let tracing_config = service_http::HttpConfig::new(
        "127.0.0.1",
        0,
        "info",
        log_format,
        args.grace_secs,
        args.body_limit_bytes,
        args.otlp_endpoint.clone(),
    );
    let tracing_identity = service_http::ServiceIdentity::new("tape", env!("CARGO_PKG_VERSION"))?;
    service_http::init_tracing_with_identity(&tracing_config, &tracing_identity)?;

    // Resolve the bearer-auth contract (#1326) BEFORE anything serves: with
    // --auth required a missing/unparseable/empty registry file is a startup
    // error (nonzero exit), never a per-request 401.
    let auth = auth::AuthConfig::resolve(
        &args.auth,
        args.token_registry_file
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .as_deref(),
        std::env::var(auth::LEGACY_TOKENS_ENV).ok().as_deref(),
    )?;
    tracing::info!(
        required = auth.required,
        "request auth resolved (TAPE_AUTH; probes stay tokenless)"
    );
    let admission = service_http::AdmissionConfig::from_env("TAPE")?.controller(
        "tape.read",
        "tape.write",
        "tape.admin",
    );
    if admission.is_some() {
        tracing::info!(
            "request admission enabled (TAPE_ADMISSION_*; probes and peer routes stay exempt)"
        );
    }

    // The operator mounts `/data` for every StatefulSet member. In its
    // single-node topology there is no Raft state machine to own durability,
    // so use that PVC for the ordinary journal store. An explicit --store
    // still wins, and replica mode continues to keep journal durability in
    // the Raft state machine instead of a second local store.
    let replica_mode = raft_runtime::cluster::replica_mode();
    let store_kind =
        resolve_journal_store(args.store.clone(), args.data_dir.as_deref(), replica_mode);
    // #3052: the WAL arm additionally needs a `probe_dir` for the periodic
    // ENOSPC re-probe and a `CommitCoordinator` wired in after the `AppState`
    // exists (its dedicated commit thread must share the exact
    // `Arc<Mutex<TapeJournal>>` the state reads through, which only exists
    // once the state is constructed).
    let (journal, legacy_store_path, wal_dir) = match &store_kind {
        JournalStoreKind::LegacyFile(path) => (load_journal(path)?, Some(path.clone()), None),
        JournalStoreKind::Wal(dir) => {
            // Never touches/deletes journal.json -- see `migrate_legacy_journal_file`'s
            // own docs for the exact no-op/rollback conditions.
            wal::migrate_legacy_journal_file(dir)?;
            let (wal_store, journal) = wal::WalStore::open(dir)?;
            (journal, None, Some((dir.clone(), wal_store)))
        }
        JournalStoreKind::None => (TapeJournal::default(), None, None),
    };
    let probe_dir = match &store_kind {
        JournalStoreKind::LegacyFile(path) => path.parent().map(std::path::Path::to_path_buf),
        JournalStoreKind::Wal(dir) => Some(dir.clone()),
        JournalStoreKind::None => None,
    };
    let mut state = AppState::with_auth(journal, legacy_store_path, auth, args.body_limit_bytes);
    if let Some((_dir, wal_store)) = wal_dir {
        let coordinator = wal::CommitCoordinator::spawn(wal_store, state.journal_handle());
        state = state.with_wal(Arc::new(coordinator));
    }
    spawn_storage_full_reprobe(state.metrics(), probe_dir);
    if let Some(path) = args.token_registry_file.as_deref() {
        // `AppState` owns this exact verifier instance, so a Secret/CSI file
        // replacement becomes visible to the live data-plane middleware
        // without restarting the Tape pod. Invalid replacements remain on the
        // shared last-known-good snapshot and emit redacted audit events.
        std::mem::drop(service_auth::spawn_registry_file_watcher(
            state.verifier(),
            path,
        ));
        tracing::info!(
            path = %path.display(),
            "watching bearer token registry for credential rotation"
        );
    }

    // Auto-mode HA (#1327): the standard downward-API quartet flips replica
    // mode (REPLICAS_PER_SHARD > 1) — no tape-specific flag. With no peer TLS
    // material the established public-port h2c topology is unchanged. A
    // complete required mTLS configuration instead makes Raft use https URLs
    // and the dedicated peer listener below.
    let mut peer_transport = None;
    let mut peer_router = None;
    if args.bootstrap_seed_uri.is_some() {
        anyhow::ensure!(
            replica_mode,
            "--bootstrap-seed-uri (TAPE_BOOTSTRAP_SEED_URI) requires replica/HA mode"
        );
        anyhow::ensure!(
            args.store.is_none(),
            "--bootstrap-seed-uri cannot be combined with --store; seed only a fresh replica PVC"
        );
    }
    if replica_mode {
        let transport = peer_tls::from_env()?
            .map(|config| peer_tls::peer_transport(&config))
            .transpose()
            .context("raft: build shared required-mTLS peer transport")?;
        let (peer_port, peer_scheme) = match transport.as_ref() {
            Some(_) => (args.raft_port, "https"),
            None => (
                args.bind
                    .rsplit(':')
                    .next()
                    .and_then(|port| port.parse::<u16>().ok())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "cannot derive the h2c raft peer port from --bind {}",
                            args.bind
                        )
                    })?,
                "http",
            ),
        };
        let topo = raft_runtime::ClusterTopology::from_env_with_scheme(
            "tape",
            &args.peer_service,
            peer_port,
            "TAPE_PEERS",
            peer_scheme,
        )?;
        let data_dir = args.data_dir.clone().ok_or_else(|| {
            anyhow::anyhow!("replica/HA mode requires a durable --data-dir (TAPE_DATA_DIR)")
        })?;
        anyhow::ensure!(
            !data_dir.as_os_str().is_empty(),
            "replica/HA mode requires a durable --data-dir (TAPE_DATA_DIR)"
        );
        if let Some(seed_uri) = args.bootstrap_seed_uri.as_deref() {
            // #2468: bootstrapSeedUri lives on the CR and is injected into
            // every pod's env, so a routine restart onto this pod's own
            // (now-populated) PVC must NOT re-attempt the seed — that would
            // crash-loop on `prepare_bootstrap_seed`'s non-empty-dir refusal.
            // Probe with the exact same emptiness check the seed path itself
            // uses, before paying for the fetch at all, and skip loudly when
            // this pod already has durable state to boot from.
            if raft::data_dir_has_existing_state(&data_dir)? {
                tracing::info!(
                    seed_uri,
                    decision = "skipped_existing_state",
                    "bootstrap seed uri set but data dir already has durable raft state; \
                     skipping seed fetch and booting from existing state"
                );
            } else {
                let bytes = service_backup::fetch_backup_object(seed_uri)
                    .with_context(|| format!("read bootstrap seed {seed_uri}"))?;
                raft::prepare_bootstrap_seed(&data_dir, topo.node_id, &bytes)?;
                tracing::info!(
                    seed_uri,
                    bytes = bytes.len(),
                    decision = "seeded",
                    "bootstrap seed prepared before raft catch-up"
                );
            }
        }
        let raft = Arc::new(match transport.clone() {
            Some(transport) => raft::TapeRaft::from_topology_with_peer_transport(
                state.journal_handle(),
                &data_dir,
                &topo,
                raft::TapeRaft::host_config(raft::SNAPSHOT_EVERY),
                transport,
            )?,
            None => raft::TapeRaft::from_topology(
                state.journal_handle(),
                &data_dir,
                &topo,
                raft::TapeRaft::host_config(raft::SNAPSHOT_EVERY),
            )?,
        });
        tracing::info!(
            node_id = topo.node_id,
            replicas = topo.replicas_per_shard,
            voters = topo.membership.voters.len(),
            peer_scheme,
            peer_port,
            "raft: replica/HA mode — append/checkpoint-put replicate"
        );
        if let Some(transport) = transport {
            peer_router = Some(raft.router());
            peer_transport = Some(transport);
        }
        state.set_raft(raft);
    }

    // Ensure CR-declared subscriptions after journal/raft is ready but before listener starts.
    // This is idempotent and tolerates AlreadyExists errors, so repeated boots converge safely.
    ensure_subscriptions(&state).await;

    let app = if peer_transport.is_some() {
        router_without_raft_routes_with_admission(state.clone(), admission)
    } else {
        router_with_admission(state.clone(), admission)
    };

    let listener = tokio::net::TcpListener::bind(&args.bind).await?;
    tracing::info!(
        addr = %listener.local_addr()?,
        "tape listening (HTTP/1.1 + HTTP/2 cleartext)"
    );

    let peer_server = match (peer_transport, peer_router) {
        (Some(transport), Some(router)) => {
            let peer_bind = peer_bind_address(&args.bind, args.raft_port)?;
            let peer_listener = tokio::net::TcpListener::bind(&peer_bind)
                .await
                .with_context(|| format!("bind authenticated raft peer listener {peer_bind}"))?;
            tracing::info!(
                addr = %peer_bind,
                tls_generation = transport.generation(),
                "tape raft peer mTLS listening"
            );
            Some(ServerTask::spawn(move |stop| async move {
                transport.serve(peer_listener, router, stop).await
            }))
        }
        (None, None) => None,
        _ => unreachable!("peer transport and router are configured together"),
    };

    let http = PublicHttp::spawn(listener, app);
    shutdown_on_signal(
        state,
        http,
        peer_server,
        Duration::from_secs(args.grace_secs),
        Duration::from_secs(args.drain_delay_secs),
    )
    .await
}

/// Reuse the public listener's host portion for the dedicated peer port.
/// This preserves `0.0.0.0`, hostname, and bracketed IPv6 bindings without
/// allowing a Raft port to silently replace the public data-plane port.
pub(crate) fn peer_bind_address(bind: &str, raft_port: u16) -> Result<String> {
    let (host, _) = bind.rsplit_once(':').ok_or_else(|| {
        anyhow::anyhow!("cannot derive authenticated raft bind address from --bind {bind}")
    })?;
    anyhow::ensure!(
        !host.is_empty(),
        "--bind must include a host before its port"
    );
    Ok(format!("{host}:{raft_port}"))
}
