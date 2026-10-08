//! Which local journal store a serving process resolves to.

use std::path::PathBuf;

/// Which durable backend a single-node serving process resolves to, given
/// `--store`, `--data-dir`, and replica mode. #3052: `--data-dir` (only
/// `serve` has this flag) now resolves to the WAL group-commit store rather
/// than the old whole-file `journal.json` under that directory; an explicit
/// `--store` keeps every offline CLI verb (and any caller that still passes
/// one to `serve`) on the unchanged legacy whole-file path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JournalStoreKind {
    /// `tape serve --data-dir <dir>`: the WAL + snapshot directory.
    Wal(PathBuf),
    /// An explicit `--store <file>` (any verb) or a serving process with
    /// neither `--store` nor `--data-dir`... this variant always carries a
    /// concrete file path; see `None` for "no journal store at all".
    LegacyFile(PathBuf),
    /// Replica mode (Raft owns durability), or neither `--store` nor
    /// `--data-dir` was given.
    None,
}

/// Resolve the local journal store for a serving process. Priority: an
/// explicit `--store` always wins (legacy whole-file path, unchanged
/// semantics); replica mode owns durability through Raft and never gets a
/// local store; otherwise `--data-dir`, when present, resolves to the WAL
/// group-commit store; absent all three, there is no local durable store.
pub(crate) fn resolve_journal_store(
    explicit_store: Option<PathBuf>,
    data_dir: Option<&std::path::Path>,
    replica_mode: bool,
) -> JournalStoreKind {
    if let Some(path) = explicit_store {
        return JournalStoreKind::LegacyFile(path);
    }
    if replica_mode {
        return JournalStoreKind::None;
    }
    match data_dir {
        Some(dir) => JournalStoreKind::Wal(dir.to_path_buf()),
        None => JournalStoreKind::None,
    }
}
