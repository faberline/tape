//! Tape's peer TLS prefix binding.
//!
//! Generic PEM parsing, validation, and rustls builders are owned by
//! `peer-tls`; reloadable peer accept/connect behavior is owned by
//! `raft-runtime`. Tape deliberately owns only the externally visible
//! environment names and this thin conversion into the shared transport.

use std::time::Duration;

use anyhow::Result;
use tokio::task::JoinHandle;

pub use peer_tls::PeerTlsConfig;

/// Prefix for Tape's peer TLS deployment contract.
pub const ENV_PREFIX: &str = "TAPE_PEER";

/// How often a running tape looks at its peer TLS files.
pub const RELOAD_INTERVAL: Duration = Duration::from_secs(30);

/// Watches the three peer TLS files and reloads `transport` when their bytes
/// change. A reload that fails (a half-written rotation, a bad certificate)
/// keeps the last good material and is tried again at the next tick.
pub fn spawn_reload(
    transport: raft_runtime::PeerTransport,
    config: PeerTlsConfig,
    interval: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut seen = read_material(&config).await.ok();
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The first tick is immediate; the files were read just above.
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let Ok(now) = read_material(&config).await else {
                tracing::warn!("raft peer TLS files unreadable; keeping the last good material");
                continue;
            };
            if seen.as_ref() == Some(&now) {
                continue;
            }
            match transport.reload(&config) {
                Ok(generation) => {
                    seen = Some(now);
                    tracing::info!(generation, "reloaded raft peer TLS material");
                }
                Err(error) => {
                    tracing::warn!(%error, "raft peer TLS reload failed; keeping the last good material");
                }
            }
        }
    })
}

/// Read the three peer TLS files, in the order cert, key, CA.
async fn read_material(config: &PeerTlsConfig) -> std::io::Result<[Vec<u8>; 3]> {
    Ok([
        tokio::fs::read(config.cert()).await?,
        tokio::fs::read(config.key()).await?,
        tokio::fs::read(config.ca()).await?,
    ])
}

/// Load Tape's peer TLS material through the shared peer-TLS adapter.
pub fn from_env() -> Result<Option<PeerTlsConfig>> {
    PeerTlsConfig::from_env(ENV_PREFIX)
}

/// Construct the shared reloadable Raft peer transport.
pub fn peer_transport(config: &PeerTlsConfig) -> Result<raft_runtime::PeerTransport> {
    raft_runtime::PeerTransport::from_config(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_material_fails_when_a_file_is_missing() {
        let missing = std::env::temp_dir().join("tape-peer-tls-missing");
        let config = PeerTlsConfig::new(
            missing.join("cert.pem"),
            missing.join("key.pem"),
            missing.join("ca.pem"),
            true,
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let result = runtime.block_on(read_material(&config));
        assert!(result.is_err(), "missing peer TLS files must not read");
    }
}
