//! `tape-bench`: the local append/replay/checkpoint benchmark and the durable
//! group-commit scaling benchmark, plus the durable-v1 JetStream comparison
//! subcommands behind the benchmark-only `jetstream-benchmark` feature.

mod cli;
#[cfg(feature = "jetstream-benchmark")]
mod jetstream;

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    cli::dispatch(cli::Cli::parse())
}
