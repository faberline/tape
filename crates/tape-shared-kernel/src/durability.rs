//! Durable-write failures and the predicate that latches degraded read-only
//! mode. Both the WAL and the legacy whole-file store report through
//! [`std::io::Error`]; this module keeps the `errno` behind one reachable.

/// Error payload that keeps a durable-write failure's `errno` reachable after
/// its [`std::io::Error`] has been rebuilt.
///
/// Rebuilding is unavoidable on this path: `storage_durable` returns
/// `anyhow::Error`, and the WAL commit coordinator has to hand one failure to every
/// waiter in a failed batch while `std::io::Error` is not `Clone`. Both
/// rebuilds go through `std::io::Error::new`, which erases `raw_os_error()` --
/// so an errno not already reflected in a *stable* [`std::io::ErrorKind`] is
/// lost. That is not hypothetical. ENOSPC survives, because it has
/// [`std::io::ErrorKind::StorageFull`]. EIO does not: it maps to
/// `ErrorKind::Uncategorized`, which is unstable and therefore unnameable in
/// stable Rust, so errno 5 is recoverable only if carried explicitly.
/// [`should_enter_storage_degraded_mode`] (WI #3052 R6) needs exactly that.
#[derive(Debug, Clone)]
pub struct DurabilityFailure {
    message: String,
    errno: Option<i32>,
}

impl std::fmt::Display for DurabilityFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DurabilityFailure {}

/// The `errno` behind a durability failure, when the write path recorded
/// one and every rebuild since has preserved it. Returns `None` for errors
/// that never came from an OS call.
pub fn durability_errno(error: &std::io::Error) -> Option<i32> {
    error
        .get_ref()
        .and_then(|source| source.downcast_ref::<DurabilityFailure>())
        .and_then(|failure| failure.errno)
}

impl DurabilityFailure {
    pub fn new(message: impl Into<String>, errno: Option<i32>) -> Self {
        Self {
            message: message.into(),
            errno,
        }
    }
}

/// EIO on both platforms this crate builds for (Linux and macOS), named here
/// rather than pulled in via a `libc` dependency for one constant.
const EIO: i32 = 5;

/// WI #3052 R6: an ENOSPC OR EIO durability failure latches sticky degraded
/// mode -- narrowing this to ENOSPC alone would route a durable EIO into a
/// plain per-request failure, exactly the "durability failure treated as an
/// ordinary retryable request" R6 forbids. Matches the accepted TD's
/// `should_enter_storage_degraded_mode` predicate. A failure that is neither
/// still fails its one request closed without flipping the server into sticky
/// read-only degraded mode.
///
/// EIO cannot be detected as `error.kind() == ErrorKind::Other &&
/// error.raw_os_error() == Some(EIO)`, which is the obvious form and is dead
/// code in both halves: EIO maps to `ErrorKind::Uncategorized` (not `Other`,
/// and unnameable in stable Rust), and every rebuild through
/// `std::io::Error::new` -- which both the WAL flattener and the commit
/// coordinator's per-waiter fan-out must do -- erases `raw_os_error()`. The
/// errno is therefore carried explicitly by [`DurabilityFailure`].
pub fn should_enter_storage_degraded_mode(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::StorageFull || durability_errno(error) == Some(EIO)
}
