//! The periodic storage-full re-probe that lifts degraded read-only mode.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tape_journal::application::TapeMetrics;

/// #2573: periodic ENOSPC re-probe — the automatic exit from degraded
/// read-only mode.
///
/// While this node is degraded (`TapeMetrics::is_storage_degraded`, set by
/// `AppState::persist` when the journal write reported
/// `io::ErrorKind::StorageFull`), try a tiny write into the journal store's
/// directory every `TAPE_STORAGE_FULL_REPROBE_SECS` (default 30s) and clear
/// the sticky flag the first time one succeeds. That is what lets a PVC that
/// was expanded or freed up recover the node WITHOUT a pod restart — an
/// operator can of course still restart it, since the flag is process-local
/// and a fresh process starts clear.
///
/// Only probes while degraded, so a healthy node pays nothing beyond one
/// timer wakeup. `probe_dir` is `None` when there is no journal store at all
/// (replica mode, or a `--store`-less process): with no durable write path
/// there is nothing to degrade, so the task is simply never spawned.
pub(crate) fn spawn_storage_full_reprobe(metrics: Arc<TapeMetrics>, probe_dir: Option<PathBuf>) {
    let Some(dir) = probe_dir else {
        return;
    };
    let reprobe_secs: u64 = std::env::var("TAPE_STORAGE_FULL_REPROBE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&v| v > 0)
        .unwrap_or(30);
    let probe_path = dir.join(".storage_full_probe");
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(reprobe_secs));
        ticker.tick().await; // discard the immediate first fire
        loop {
            ticker.tick().await;
            if !metrics.is_storage_degraded() {
                continue;
            }
            match tokio::fs::write(&probe_path, b"ok").await {
                Ok(()) => {
                    metrics.clear_storage_degraded();
                    tracing::info!(
                        path = %probe_path.display(),
                        "storage re-probe write succeeded; leaving degraded read-only mode"
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        path = %probe_path.display(),
                        "storage re-probe still failing; staying in degraded read-only mode"
                    );
                }
            }
        }
    });
}
