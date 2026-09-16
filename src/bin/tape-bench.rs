// <HANDWRITE gap="missing-generator:logic:tape-competitor-performance" tracker="#768" reason="Initial benchmark CLI before generated efficiency runner primitives exist.">
use anyhow::{bail, Result};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(name = "tape-bench", version, about = "Tape local benchmark runner")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the local Tape replay benchmark and report win/loss calibration status.
    Run(RunArgs),
    /// WI #3052 AC1: drive the real WAL commit coordinator over real HTTP at
    /// varying connection counts and report the durable throughput scaling
    /// ratio (highest sampled connection count vs. the lowest).
    Durable(DurableArgs),
    /// Drive one fixed durable-v1 cell against a real Tape or JetStream endpoint.
    #[cfg(feature = "jetstream-benchmark")]
    JetstreamClient(JetstreamClientArgs),
    /// Run the complete durable-v1 cell matrix and emit one JSON object per line.
    #[cfg(feature = "jetstream-benchmark")]
    JetstreamSuite(JetstreamSuiteArgs),
}

#[cfg(feature = "jetstream-benchmark")]
#[derive(clap::Args)]
struct JetstreamClientArgs {
    #[arg(long, default_value = "durable-v1")]
    profile: String,
    #[arg(long, value_parser = ["normal", "recovery"], default_value = "normal")]
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

#[cfg(feature = "jetstream-benchmark")]
#[derive(clap::Args)]
struct JetstreamSuiteArgs {
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

#[derive(clap::Args)]
struct RunArgs {
    /// Number of local events to append and replay.
    #[arg(long, default_value_t = 1_000)]
    events: usize,
    /// Payload body size in bytes.
    #[arg(long, default_value_t = 128)]
    payload_bytes: usize,
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(clap::Args)]
struct DurableArgs {
    /// Number of sequential append requests each connection issues.
    #[arg(long, default_value_t = 200)]
    events_per_connection: usize,
    /// Payload body size in bytes.
    #[arg(long, default_value_t = 128)]
    payload_bytes: usize,
    /// Connection counts to sample, e.g. `--connections 1,4,16`.
    #[arg(long, value_delimiter = ',', default_value = "1,4,16")]
    connections: Vec<usize>,
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    format: OutputFormat,
}

#[derive(Clone, Copy, ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run(args) => run(args),
        Command::Durable(args) => durable(args),
        #[cfg(feature = "jetstream-benchmark")]
        Command::JetstreamClient(args) => jetstream_client(args),
        #[cfg(feature = "jetstream-benchmark")]
        Command::JetstreamSuite(args) => jetstream_suite(args),
    }
}

#[cfg(feature = "jetstream-benchmark")]
fn jetstream_suite(args: JetstreamSuiteArgs) -> Result<()> {
    let cells = tape::bench::jetstream_client::matrix_cells(&args.phase).map_err(anyhow::Error::msg)?;
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let mut failed: Option<String> = None;
    for (payload_bytes, clients, sample_index) in cells {
        let targets: &[&str] = if args.target == "both" { &["tape", "jetstream"] } else { &[args.target.as_str()] };
        for target in targets {
            let config = tape::bench::jetstream_client::ClientConfig { tape_url: args.tape_url.clone(), nats_url: args.nats_url.clone(), topic: format!("{}-{}-{}-{}", args.topic, payload_bytes, clients, sample_index), payload_bytes, clients, sample_index, operations_per_client: args.operations_per_client, replay_events: 100_000, sample_seconds: 60, profile: args.profile.clone(), phase: args.phase.clone(), run_id: args.run_id.clone() };
            let record = if let Some(cause) = failed.clone() {
                tape::bench::jetstream_client::incomplete_record(&config, if *target == "tape" { "tape" } else { "jetstream" }, cause)
            } else {
                match runtime.block_on(async { if *target == "tape" { tape::bench::jetstream_client::run_tape(config.clone()).await } else { tape::bench::jetstream_client::run(config.clone()).await } }) {
                Ok(record) => record,
                Err(error) => {
                    let cause = format!("{} cell {} payload={} clients={} failed: {error:#}", target, sample_index, payload_bytes, clients);
                    failed = Some(cause.clone());
                    tape::bench::jetstream_client::incomplete_record(&config, if *target == "tape" { "tape" } else { "jetstream" }, cause)
                }
                }
            };
            println!("{}", serde_json::to_string(&record)?);
        }
    }
    if let Some(cause) = failed {
        bail!("durable-v1 suite incomplete: {cause}");
    }
    Ok(())
}


#[cfg(feature = "jetstream-benchmark")]
fn jetstream_client(args: JetstreamClientArgs) -> Result<()> {
    let config = tape::bench::jetstream_client::ClientConfig {
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
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let record = runtime.block_on(async {
        if args.target == "tape" {
            tape::bench::jetstream_client::run_tape(config).await
        } else {
            tape::bench::jetstream_client::run(config).await
        }
    })?;
    println!("{}", serde_json::to_string(&record)?);
    Ok(())
}

fn run(args: RunArgs) -> Result<()> {
    let report = tape::bench::run_benchmark(args.events, args.payload_bytes);
    match args.format {
        OutputFormat::Text => {
            println!(
                "events={} payload_bytes={} append_p50_us={} append_p95_us={} replay_full_us={} checkpoint_p50_us={} checkpoint_p95_us={} verdict={}",
                report.events,
                report.payload_bytes,
                report.append_p50_us,
                report.append_p95_us,
                report.replay_full_us,
                report.checkpoint_p50_us,
                report.checkpoint_p95_us,
                report.verdict
            );
            for peer in &report.peers {
                println!(
                    "peer={} replay_baseline={} status={} win_claim={} reason={}",
                    peer.peer, peer.replay_baseline, peer.status, peer.win_claim, peer.reason
                );
            }
        }
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
    }
    if let Err(error) = tape::bench::verify_report(&report) {
        bail!("{error}");
    }
    Ok(())
}

fn durable(args: DurableArgs) -> Result<()> {
    if args.connections.is_empty() {
        bail!("--connections requires at least one connection count");
    }
    let report = tape::bench::run_durable_benchmark(
        args.events_per_connection,
        args.payload_bytes,
        &args.connections,
    );
    match args.format {
        OutputFormat::Text => {
            println!(
                "payload_bytes={} scaling_ratio={:.2}x",
                report.payload_bytes, report.scaling_ratio
            );
            for sample in &report.samples {
                println!(
                    "connections={} events={} elapsed_us={} ops_per_sec={:.2}",
                    sample.connections, sample.events, sample.elapsed_us, sample.ops_per_sec
                );
            }
        }
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
    }
    Ok(())
}
// </HANDWRITE>
