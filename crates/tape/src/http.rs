//! The HTTP assembly: [`AppState`] wires the journal service onto its durable
//! backend, auth, and the optional raft group; [`router`] composes the
//! journal data plane, the shared probe routes, and the raft peer routes into
//! one axum app.

mod app_state;
mod router;

#[cfg(test)]
mod recovery_tests;
#[cfg(test)]
mod tests;

pub use app_state::AppState;
pub use router::{
    router, router_with_admission, router_without_raft_routes,
    router_without_raft_routes_with_admission,
};
