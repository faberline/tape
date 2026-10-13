//! The benchmark-only durable-v1 subcommands (`--features jetstream-benchmark`):
//! `jetstream-client` drives one cell, `jetstream-suite` the whole matrix.

use anyhow::{bail, Result};
use tape_bench::jetstream_client::{self, ClientConfig};

#[derive(clap::Args)]
pub(crate) struct ClientArgs {
    #[arg(long, default_value = "durable-v1")]
    profile: String,
    /// `finalize` writes and verifies the fixed persisted replay backlog.
    #[arg(long, value_parser = ["normal", "finalize", "recovery"], default_value = "normal")]
    phase: String,
    #[arg(long, value_parser = ["tape", "jetstream"], default_value = "jetstream")]
    target: String,
    #[arg(long, default_value = "http://127.0.0.1:7137")]
    tape_url: String,
    #[arg(long, default_value = "nats://127.0.0.1:4222")]
    nats_url: String,
    #[arg(long, default_value = "bench")]
    topic: String,
    #[arg(long, default_value = "manual")]
    run_id: String,
    #[arg(long)]
    payload_bytes: usize,
    #[arg(long)]
    clients: usize,
    #[arg(long)]
    sample_index: usize,
    #[arg(long, default_value_t = 1_000)]
    operations_per_client: usize,
    #[arg(long, default_value_t = 100_000)]
    replay_events: usize,
    #[arg(long, default_value_t = 60)]
    sample_seconds: u64,
}

#[derive(clap::Args)]
pub(crate) struct SuiteArgs {
    #[arg(long, value_parser = ["tape", "jetstream", "both"])]
    target: String,
    #[arg(long, default_value = "durable-v1")]
    profile: String,
    #[arg(long, value_parser = ["normal", "recovery"], default_value = "normal")]
    phase: String,
    #[arg(long, default_value = "http://127.0.0.1:7137")]
    tape_url: String,
    #[arg(long, default_value = "nats://127.0.0.1:4222")]
    nats_url: String,
    #[arg(long, default_value = "bench")]
    topic: String,
    #[arg(long, default_value = "manual")]
    run_id: String,
    #[arg(long, default_value_t = 1_000)]
    operations_per_client: usize,
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?)
}

async fn run_target(
    target: &str,
    config: ClientConfig,
) -> Result<jetstream_client::BenchmarkRecord> {
    if target == "tape" {
        jetstream_client::run_tape(config).await
    } else {
        jetstream_client::run(config).await
    }
}

fn record_target(target: &str) -> &'static str {
    if target == "tape" {
        "tape"
    } else {
        "jetstream"
    }
}

/// Run the complete durable-v1 cell matrix and emit one JSON object per line.
/// After the first failed cell every later cell is recorded as incomplete
/// with that cause rather than rerun.
pub(crate) fn suite(args: SuiteArgs) -> Result<()> {
    let cells = jetstream_client::matrix_cells(&args.phase).map_err(anyhow::Error::msg)?;
    let runtime = runtime()?;
    let mut failed: Option<String> = None;
    for (payload_bytes, clients, sample_index) in cells {
        let targets: &[&str] = if args.target == "both" {
            &["tape", "jetstream"]
        } else {
            &[args.target.as_str()]
        };
        for target in targets {
            let config = ClientConfig {
                tape_url: args.tape_url.clone(),
                nats_url: args.nats_url.clone(),
                topic: format!(
                    "{}-{}-{}-{}",
                    args.topic, payload_bytes, clients, sample_index
                ),
                payload_bytes,
                clients,
                sample_index,
                operations_per_client: args.operations_per_client,
                replay_events: 100_000,
                sample_seconds: 60,
                profile: args.profile.clone(),
                phase: args.phase.clone(),
                run_id: args.run_id.clone(),
            };
            let record = match &failed {
                Some(cause) => jetstream_client::incomplete_record(
                    &config,
                    record_target(target),
                    cause.clone(),
                ),
                None => match runtime.block_on(run_target(target, config.clone())) {
                    Ok(record) => record,
                    Err(error) => {
                        let cause = format!(
                            "{target} cell {sample_index} payload={payload_bytes} clients={clients} failed: {error:#}"
                        );
                        failed = Some(cause.clone());
                        jetstream_client::incomplete_record(&config, record_target(target), cause)
                    }
                },
            };
            println!("{}", serde_json::to_string(&record)?);
        }
    }
    if let Some(cause) = failed {
        bail!("durable-v1 suite incomplete: {cause}");
    }
    Ok(())
}

/// Drive one fixed durable-v1 cell against a real Tape or JetStream endpoint.
pub(crate) fn client(args: ClientArgs) -> Result<()> {
    let config = ClientConfig {
        tape_url: args.tape_url,
        nats_url: args.nats_url,
        topic: args.topic,
        payload_bytes: args.payload_bytes,
        clients: args.clients,
        sample_index: args.sample_index,
        operations_per_client: args.operations_per_client,
        replay_events: args.replay_events,
        sample_seconds: args.sample_seconds,
        profile: args.profile,
        phase: args.phase,
        run_id: args.run_id,
    };
    let record = runtime()?.block_on(run_target(&args.target, config))?;
    println!("{}", serde_json::to_string(&record)?);
    Ok(())
}
