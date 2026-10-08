use std::sync::{Arc, Mutex};

use super::*;

/// Build a log whose `--store` points at `path`, carrying one event on
/// `topic` so the persisted journal has content worth losing.
fn log_with_store(path: &std::path::Path, topic: &str) -> FileLog {
    let mut journal = TapeJournal::default();
    journal.append_at(topic, None, serde_json::json!({ "n": 1 }), 100, 100);
    FileLog::new(Arc::new(Mutex::new(journal)), Some(path.to_path_buf()))
}

/// The staging path `storage_durable::atomic_write` uses for `path` —
/// the whole path with `.tmp` appended, not an extension swap.
fn staging_path(path: &std::path::Path) -> std::path::PathBuf {
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    tmp.into()
}

/// #2572 — a failed write must leave the PREVIOUS journal intact.
///
/// The failure is induced by occupying the temp path with a directory, so
/// `atomic_write` cannot create its temp file. That is a stand-in for the
/// real motivating cases (crash mid-write, pod eviction, ENOSPC) which are
/// not deterministically reproducible in a unit test — what it reproduces
/// faithfully is the property under test: the write fails *before* the
/// live file is touched.
///
/// Verified to fail against the pre-#2572 implementation, at the
/// `expect_err`: `fs::write` ignores the staging path entirely, so it
/// reported success and replaced the journal it could not safely write.
#[test]
fn persist_failure_leaves_the_previous_journal_intact() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.json");

    // Establish a good journal on disk.
    let first = log_with_store(&path, "orders");
    first
        .persist(&first.journal.lock().unwrap())
        .expect("first persist writes the journal");
    let good = std::fs::read(&path).expect("journal is on disk");
    assert!(!good.is_empty());

    // Block the temp path, then try to persist different content.
    std::fs::create_dir(staging_path(&path)).unwrap();
    let second = log_with_store(&path, "shipments");
    let error = second
        .persist(&second.journal.lock().unwrap())
        .expect_err("persist must fail when it cannot stage the write");

    // The live journal is byte-identical and still parses.
    assert_eq!(
        std::fs::read(&path).unwrap(),
        good,
        "a failed persist must not modify the live journal; error was: {error}"
    );
    let reloaded: TapeJournal = serde_json::from_slice(&good).expect("journal still parses");
    assert_eq!(
        reloaded.replay("orders", None, None, None).len(),
        1,
        "the surviving journal is the original one, not the failed write"
    );
    assert!(
        reloaded.replay("shipments", None, None, None).is_empty(),
        "the failed write left no trace in the live journal"
    );
}

/// #2572 — a successful persist commits by rename and leaves no residue.
/// A leftover `.tmp` would mean the rename did not happen and the next
/// boot could find two candidate files.
///
/// Unlike the test above this one also passed pre-#2572 (`fs::write`
/// never creates a staging file to leave behind). It is not a regression
/// guard for the old bug; it guards the new mechanism — that the commit
/// path stays a rename, and that parent-directory creation survived the
/// move into `atomic_write`.
#[test]
fn persist_commits_by_rename_without_temp_residue() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("journal.json");

    let log = log_with_store(&path, "orders");
    log.persist(&log.journal.lock().unwrap())
        .expect("persist creates parent directories and writes");

    assert!(path.exists(), "the journal is at its final path");
    assert!(
        !staging_path(&path).exists(),
        "the temp file was renamed into place, not left behind"
    );
    let reloaded: TapeJournal = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(reloaded.replay("orders", None, None, None).len(), 1);
}
