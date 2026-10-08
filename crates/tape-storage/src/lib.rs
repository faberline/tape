//! Single-node durable backends behind the journal's
//! [`CommitLog`](tape_shared_kernel::CommitLog) port: the group-commit
//! write-ahead log ([`wal`]) and the legacy whole-file store ([`file_log`]).

pub mod file_log;
pub mod wal;
