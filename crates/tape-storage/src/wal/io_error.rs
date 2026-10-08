//! Rebuilding `std::io::Error`s without losing the kind or errno the
//! degraded-mode predicate reads.

use tape_shared_kernel::{durability_errno, DurabilityFailure};

/// Collapse an `anyhow::Error` from a `storage_durable` call back into a
/// `std::io::Error` without losing its [`std::io::ErrorKind`], the same
/// discipline the legacy file log's `flatten_atomic_write_error` uses
/// for exactly this reason: a caller (WI #3052 step 3) needs to discriminate
/// ENOSPC/EIO from an ordinary failure, which a bare `anyhow` chain loses.
pub(super) fn flatten_io_error(error: anyhow::Error) -> std::io::Error {
    match error.downcast_ref::<std::io::Error>() {
        Some(source) => std::io::Error::new(
            source.kind(),
            DurabilityFailure::new(format!("{error:#}"), source.raw_os_error()),
        ),
        None => std::io::Error::other(format!("{error:#}")),
    }
}

/// Rebuild an [`std::io::Error`] preserving both its [`std::io::ErrorKind`]
/// and any errno [`durability_errno`] can read back, since `std::io::Error`
/// is not `Clone` and one failed batch has many waiters.
pub(super) fn clone_io_error(error: &std::io::Error) -> std::io::Error {
    std::io::Error::new(
        error.kind(),
        DurabilityFailure::new(error.to_string(), durability_errno(error)),
    )
}

/// A `serde_json` encode/decode failure is corruption or a programmer error,
/// never a durability signal -- map it to `InvalidData` rather than `Other`
/// so it is at least distinguishable from an I/O failure.
pub(super) fn json_err(error: serde_json::Error) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error)
}
