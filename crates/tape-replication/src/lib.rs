//! Raft replication for the journal: the raft group that implements the
//! [`Replicator`](tape_shared_kernel::Replicator) port ([`raft`]) and its
//! peer mTLS configuration ([`peer_tls`]).

pub mod peer_tls;
pub mod raft;
