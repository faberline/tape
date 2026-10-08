//! The offline journal verbs (`append`, `replay`, `checkpoint`,
//! `subscription`): load the whole-file `--store`, apply one operation,
//! save it back.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Subcommand;
use serde_json::{json, Value};
use tape_journal::application::JournalClock;
use tape_shared_kernel::TapeJournal;

#[derive(clap::Args)]
pub(crate) struct AppendArgs {
    /// Topic name.
    pub(crate) topic: String,
    /// Optional partitioning/idempotency key carried in the event envelope.
    #[arg(long)]
    pub(crate) key: Option<String>,
    /// JSON payload or a string payload when the value is not valid JSON.
    #[arg(long)]
    pub(crate) payload: String,
    /// Override event timestamp for deterministic tests/backfill.
    #[arg(long)]
    pub(crate) timestamp_ms: Option<u64>,
    /// Journal file. Defaults to `.tape/journal.json`.
    #[arg(long, default_value = ".tape/journal.json")]
    pub(crate) store: PathBuf,
}

#[derive(clap::Args)]
pub(crate) struct ReplayArgs {
    /// Topic name.
    pub(crate) topic: String,
    /// First offset to include.
    #[arg(long)]
    pub(crate) from_offset: Option<u64>,
    /// First event timestamp to include.
    #[arg(long)]
    pub(crate) from_timestamp_ms: Option<u64>,
    /// Maximum number of events to return. Omitted returns at most 1000
    /// oldest-first events (#2484); page with `--from-offset`/`--limit` to
    /// read past that window.
    #[arg(long)]
    pub(crate) limit: Option<usize>,
    /// Journal file. Defaults to `.tape/journal.json`.
    #[arg(long, default_value = ".tape/journal.json")]
    pub(crate) store: PathBuf,
}

#[derive(clap::Args)]
pub(crate) struct CheckpointArgs {
    #[command(subcommand)]
    pub(crate) command: CheckpointCommand,
}

#[derive(Subcommand)]
pub(crate) enum CheckpointCommand {
    /// Read a consumer checkpoint.
    Get(CheckpointGetArgs),
    /// Advance a consumer checkpoint.
    Put(CheckpointPutArgs),
}

#[derive(clap::Args)]
pub(crate) struct CheckpointGetArgs {
    pub(crate) topic: String,
    pub(crate) consumer: String,
    #[arg(long, default_value = ".tape/journal.json")]
    pub(crate) store: PathBuf,
}

#[derive(clap::Args)]
pub(crate) struct CheckpointPutArgs {
    pub(crate) topic: String,
    pub(crate) consumer: String,
    #[arg(long)]
    pub(crate) offset: u64,
    #[arg(long, default_value = ".tape/journal.json")]
    pub(crate) store: PathBuf,
}

#[derive(clap::Args)]
pub(crate) struct SubscriptionArgs {
    #[command(subcommand)]
    pub(crate) command: SubscriptionCommand,
}

#[derive(Subcommand)]
pub(crate) enum SubscriptionCommand {
    /// Create a caller-driven pull subscription.
    Create(SubscriptionCreateArgs),
    /// List the delivery resources for one topic.
    List(SubscriptionListArgs),
    /// Show one topic delivery resource and its pull checkpoint, if any.
    Show(SubscriptionShowArgs),
    /// Read one bounded event window from a pull subscription cursor.
    Pull(SubscriptionPullArgs),
    /// Advance a pull subscription cursor after the caller processes a window.
    Ack(SubscriptionAckArgs),
    /// Delete resource metadata while preserving a matching pull checkpoint.
    Delete(SubscriptionDeleteArgs),
}

#[derive(clap::Args)]
pub(crate) struct SubscriptionCreateArgs {
    /// Topic that owns the subscription.
    pub(crate) topic: String,
    /// Subscription name, also used as the durable checkpoint consumer name.
    pub(crate) name: String,
    /// Journal file. Defaults to `.tape/journal.json`.
    #[arg(long, default_value = ".tape/journal.json")]
    pub(crate) store: PathBuf,
}

#[derive(clap::Args)]
pub(crate) struct SubscriptionListArgs {
    pub(crate) topic: String,
    #[arg(long, default_value = ".tape/journal.json")]
    pub(crate) store: PathBuf,
}

#[derive(clap::Args)]
pub(crate) struct SubscriptionShowArgs {
    pub(crate) topic: String,
    pub(crate) name: String,
    #[arg(long, default_value = ".tape/journal.json")]
    pub(crate) store: PathBuf,
}

#[derive(clap::Args)]
pub(crate) struct SubscriptionPullArgs {
    pub(crate) topic: String,
    pub(crate) name: String,
    /// Maximum events to return; defaults to the bounded pull window.
    #[arg(long)]
    pub(crate) limit: Option<usize>,
    #[arg(long, default_value = ".tape/journal.json")]
    pub(crate) store: PathBuf,
}

#[derive(clap::Args)]
pub(crate) struct SubscriptionAckArgs {
    pub(crate) topic: String,
    pub(crate) name: String,
    /// Next unread offset after the events this caller processed.
    #[arg(long)]
    pub(crate) offset: u64,
    #[arg(long, default_value = ".tape/journal.json")]
    pub(crate) store: PathBuf,
}

#[derive(clap::Args)]
pub(crate) struct SubscriptionDeleteArgs {
    pub(crate) topic: String,
    pub(crate) name: String,
    #[arg(long, default_value = ".tape/journal.json")]
    pub(crate) store: PathBuf,
}

pub(crate) fn append(args: AppendArgs) -> Result<()> {
    let mut journal = load_journal(&args.store)?;
    let payload = parse_payload(&args.payload);
    let event = journal.append(args.topic, args.key, payload, args.timestamp_ms);
    save_journal(&args.store, &journal)?;
    println!("{}", serde_json::to_string_pretty(&event)?);
    println!(
        "next: tape replay {} --from-offset {}",
        event.topic, event.offset
    );
    Ok(())
}

pub(crate) fn replay(args: ReplayArgs) -> Result<()> {
    let journal = load_journal(&args.store)?;
    let events = journal.replay(
        &args.topic,
        args.from_offset,
        args.from_timestamp_ms,
        args.limit,
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({ "events": events }))?
    );
    println!("next: done");
    Ok(())
}

pub(crate) fn checkpoint(args: CheckpointArgs) -> Result<()> {
    match args.command {
        CheckpointCommand::Get(args) => {
            let journal = load_journal(&args.store)?;
            let checkpoint = journal.checkpoint(&args.topic, &args.consumer);
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({ "checkpoint": checkpoint }))?
            );
            println!("next: done");
            Ok(())
        }
        CheckpointCommand::Put(args) => {
            let mut journal = load_journal(&args.store)?;
            let checkpoint = journal.put_checkpoint(args.topic, args.consumer, args.offset)?;
            save_journal(&args.store, &journal)?;
            println!("{}", serde_json::to_string_pretty(&checkpoint)?);
            println!("next: done");
            Ok(())
        }
    }
}

pub(crate) fn subscription(args: SubscriptionArgs) -> Result<()> {
    match args.command {
        SubscriptionCommand::Create(args) => {
            let mut journal = load_journal(&args.store)?;
            let subscription = journal.create_subscription(args.topic, args.name)?;
            save_journal(&args.store, &journal)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({ "subscription": subscription }))?
            );
            println!(
                "next: tape subscription show {} {} --store {}",
                subscription.topic,
                subscription.name,
                args.store.display()
            );
            Ok(())
        }
        SubscriptionCommand::List(args) => {
            let journal = load_journal(&args.store)?;
            let subscriptions = journal.subscriptions(&args.topic);
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({ "subscriptions": subscriptions }))?
            );
            println!("next: done");
            Ok(())
        }
        SubscriptionCommand::Show(args) => {
            let journal = load_journal(&args.store)?;
            let subscription = journal
                .subscription(&args.topic, &args.name)
                .cloned()
                .with_context(|| {
                    format!(
                        "subscription {} does not exist for topic {}",
                        args.name, args.topic
                    )
                })?;
            let checkpoint = journal.checkpoint(&subscription.topic, &subscription.name);
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &json!({ "subscription": subscription, "checkpoint": checkpoint })
                )?
            );
            println!("next: done");
            Ok(())
        }
        SubscriptionCommand::Pull(args) => {
            let journal = load_journal(&args.store)?;
            let batch = journal.pull_subscription(&args.topic, &args.name, args.limit)?;
            let next = if batch.events.is_empty() {
                "done".to_string()
            } else {
                format!(
                    "tape subscription ack {} {} --offset {} --store {}",
                    batch.topic,
                    batch.subscription,
                    batch.next_offset,
                    args.store.display()
                )
            };
            println!("{}", serde_json::to_string_pretty(&batch)?);
            println!("next: {next}");
            Ok(())
        }
        SubscriptionCommand::Ack(args) => {
            let mut journal = load_journal(&args.store)?;
            let checkpoint = journal.ack_subscription(&args.topic, &args.name, args.offset)?;
            save_journal(&args.store, &journal)?;
            println!("{}", serde_json::to_string_pretty(&checkpoint)?);
            println!(
                "next: tape subscription pull {} {} --store {}",
                args.topic,
                args.name,
                args.store.display()
            );
            Ok(())
        }
        SubscriptionCommand::Delete(args) => {
            let mut journal = load_journal(&args.store)?;
            let subscription = journal.delete_subscription(&args.topic, &args.name)?;
            save_journal(&args.store, &journal)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({ "deleted": subscription }))?
            );
            println!(
                "next: tape subscription list {} --store {}",
                args.topic,
                args.store.display()
            );
            Ok(())
        }
    }
}

pub(crate) fn load_journal(path: &Path) -> Result<TapeJournal> {
    if !path.exists() {
        return Ok(TapeJournal::default());
    }
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

/// Write the journal to `path` durably (#2572).
///
/// Goes through [`storage_durable::atomic_write`] — temp file, fsync, rename,
/// parent directory fsync — so an interrupted or failed write leaves the
/// previous journal intact and loadable. The plain `fs::write` this replaces
/// truncated the file before writing and never fsynced, so a crash or a full
/// disk destroyed the journal it was trying to update.
///
/// [`FsyncPolicy::Always`] matches `AppState::persist`: this file is the only
/// durability guarantee the single-node path has.
pub(crate) fn save_journal(path: &Path, journal: &TapeJournal) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(journal)?;
    storage_durable::atomic_write(path, &bytes, storage_durable::FsyncPolicy::Always)
        .with_context(|| format!("write {}", path.display()))
}

pub(crate) fn parse_payload(input: &str) -> Value {
    serde_json::from_str(input).unwrap_or_else(|_| Value::String(input.to_string()))
}
