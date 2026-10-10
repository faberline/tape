//! `tape`: the offline journal verbs, the HTTP server, and the offline
//! spec/deploy-artifact renders, all in one binary.

mod backup;
mod cli;
mod connect;
mod dockerfile;
mod issue;
mod k8s;
mod llm;
mod offline;
mod serve;
mod spec;
#[cfg(test)]
mod tests;

use anyhow::Result;
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    // kube-rs (operator), raft peer TLS, and online CLI paths can link
    // different rustls providers. Install the shared aws-lc-rs default before
    // any of those paths construct a TLS client or server.
    peer_tls::install_default_crypto_provider();
    cli::dispatch(cli::Cli::parse()).await
}
