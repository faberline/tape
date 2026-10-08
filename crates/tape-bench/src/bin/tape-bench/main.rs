//! `tape-bench`: the local append/replay/checkpoint benchmark and the durable
//! group-commit scaling benchmark.

mod cli;

use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    cli::dispatch(cli::Cli::parse())
}
