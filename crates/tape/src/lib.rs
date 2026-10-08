//! tape: a durable, replayable topic journal with consumer checkpoints and
//! pull subscriptions, served over HTTP and replicated through raft.
//!
//! This crate is the assembly: it wires the contexts (see
//! `docs/architecture.md` and `ddd.toml`) into what the `tape` binary runs —
//! the HTTP app ([`http`]) and the `tape backup` runner (`backup`, feature
//! `backup`).

#[cfg(feature = "backup")]
pub mod backup;
pub mod http;
