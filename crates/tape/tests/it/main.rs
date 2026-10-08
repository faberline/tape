//! Integration tests: one module per contract area, linked into a single
//! test binary. Feature-gated areas compile only with their feature
//! (`cargo test --features operator,backup` runs everything).

mod audit_contract;
#[cfg(feature = "backup")]
mod backup;
mod backup_destination_docs;
mod bootstrap;
mod cli_contract;
mod deploy_cli;
mod direct_k8s_assets;
mod durable_crash_recovery;
mod durable_write_path;
mod http_transport;
mod long_running_stability;
mod network_policy_assets;
mod observability_assets;
#[cfg(feature = "operator")]
mod operator;
#[cfg(feature = "operator")]
mod operator_render_provision_topics;
mod provision_topics_via_spec;
mod raft_cluster;
mod raft_failover;
mod raft_peer_mtls;
mod raft_persistence;
mod retention_backfill;
mod rig_stateful_adapter;
mod seed_ha_bootstrap;
mod service_admission;
mod service_auth;
mod shared_otlp_tracing;
mod spec_generated_clients;
mod spec_route_parity;
