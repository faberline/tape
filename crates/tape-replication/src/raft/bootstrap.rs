//! Cold-start seeding of an empty replica from one `/admin/backup` object.

use std::path::Path;

use anyhow::{Context, Result};
use raft_runtime::{FsyncPolicy, NodeId, RaftStore};

use tape_shared_kernel::JournalSnapshot;

/// True when `data_dir` already carries durable raft state, i.e. it exists
/// and contains any entry other than the innocuous ext4 `lost+found` a
/// freshly provisioned cloud PV mounts (#2443).
///
/// This is the single emptiness probe [`prepare_bootstrap_seed`] uses for its
/// own cold-start refusal, extracted so a caller can consult the exact same
/// answer *before* deciding whether to seed at all (#2468): `bootstrapSeedUri`
/// lives on the CR and is injected into every pod's env, so without this
/// pre-check every routine pod replacement re-runs the seed path and
/// crash-loops on `prepare_bootstrap_seed`'s refusal. A caller that skips
/// seeding when this returns `true` turns the field into declarative
/// "bootstrap if empty" instead of one-shot-then-must-remove, while
/// `prepare_bootstrap_seed` keeps refusing a populated dir as the last line
/// of defense.
pub fn data_dir_has_existing_state(data_dir: &Path) -> Result<bool> {
    if !data_dir.exists() {
        return Ok(false);
    }
    let entries = std::fs::read_dir(data_dir)
        .with_context(|| format!("read data dir {}", data_dir.display()))?;
    for entry in entries {
        let entry = entry?;
        // A freshly provisioned cloud PV mounts its ext4 filesystem root
        // directly at the data dir, so `lost+found` is present on every real
        // PVC (#2443). It is mkfs output, not raft state.
        if entry.file_name() == "lost+found" {
            continue;
        }
        return Ok(true);
    }
    Ok(false)
}

/// Prepare an empty replica PVC to recover from one exact backup object.
///
/// This is deliberately a cold-start-only operation: it refuses any existing
/// content in `data_dir` (see [`data_dir_has_existing_state`]), decodes the
/// same [`JournalSnapshot`] shape served by `/admin/backup`, then writes the
/// state-machine files that [`super::TapeRaft::from_topology`] will consume. The
/// snapshot is installed before the Raft host opens its store, so normal Raft
/// log/snapshot catch-up resumes from `up_to` instead of replaying the seed
/// as new appends. It is not a live restore API and must never overwrite a
/// running replica's durable state. Callers that want restart-tolerant
/// "bootstrap if empty" semantics (#2468) must consult
/// [`data_dir_has_existing_state`] themselves before calling this function;
/// it still refuses unconditionally so a mis-set seed URI on a dirty dir
/// stays a loud error rather than a silent no-op.
pub fn prepare_bootstrap_seed(data_dir: &Path, node_id: NodeId, bytes: &[u8]) -> Result<()> {
    let snapshot: JournalSnapshot =
        serde_json::from_slice(bytes).context("decode bootstrap JournalSnapshot JSON")?;

    if data_dir_has_existing_state(data_dir)? {
        anyhow::bail!(
            "bootstrap seed requires an empty data directory {}; refusing to replace existing raft state",
            data_dir.display()
        );
    }
    if !data_dir.exists() {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("create bootstrap data dir {}", data_dir.display()))?;
    }

    // A single-node tape has no raft applied index, so its `/admin/backup`
    // snapshot honestly carries `up_to: 0` — but raft treats a snapshot at
    // index 0 as nonexistent, so seeding it verbatim boots a Ready replica
    // group with an EMPTY journal (#2465; GKE runs 0723113842/0723120246).
    // Install such a seed at a real genesis index instead, rewriting the
    // embedded `up_to` so the raft snapshot metadata and the state machine
    // agree on every identically-seeded member.
    let (seed_index, seed_bytes) = if snapshot.up_to == 0 {
        let genesis = JournalSnapshot {
            up_to: 1,
            journal: snapshot.journal,
            completed_proposals: snapshot.completed_proposals,
        };
        (
            1,
            serde_json::to_vec(&genesis).context("re-encode genesis bootstrap snapshot")?,
        )
    } else {
        (snapshot.up_to, bytes.to_vec())
    };

    let raft_dir = data_dir.join("raft");
    let store = RaftStore::open(
        raft_dir
            .to_str()
            .context("raft data dir is not valid UTF-8")?,
        node_id,
        FsyncPolicy::Always,
    )?;
    store.seed_snapshot(seed_index, 0, seed_bytes)?;
    Ok(())
}
