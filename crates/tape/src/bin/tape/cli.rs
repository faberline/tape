//! The `tape` command surface: the top-level verbs and their dispatch.

use std::path::Path;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::backup::BackupArgs;
use crate::dockerfile::DockerfileArgs;
use crate::issue::IssueArgs;
use crate::k8s::K8sArgs;
use crate::llm::LlmArgs;
use crate::offline::{AppendArgs, CheckpointArgs, ReplayArgs, SubscriptionArgs};
use crate::serve::ServeArgs;
use crate::spec::SpecArgs;

#[derive(Parser)]
#[command(name = "tape", version, about = "tape - topic replay journal service")]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Append one event envelope to a topic journal.
    Append(AppendArgs),
    /// Replay topic history by offset or timestamp.
    Replay(ReplayArgs),
    /// Manage durable consumer replay checkpoints.
    Checkpoint(CheckpointArgs),
    /// Manage named caller-driven topic pull cursors.
    Subscription(SubscriptionArgs),
    /// Serve the topic journal over HTTP (h2c + HTTP/1.1 on one port).
    Serve(ServeArgs),
    /// Print Tape's machine-readable API contract, offline.
    Spec(SpecArgs),
    /// Print agent-facing LLM topics, offline.
    Llm(LlmArgs),
    /// Self-update this binary from a published GitHub release.
    Upgrade(UpgradeArgs),
    /// Search, view, file, and comment on Tape issues.
    Issue(IssueArgs),
    /// Kubernetes artifacts split by layer: the cluster-scoped CRD, the
    /// operator control plane, and app-namespace Tape instances. Render paths
    /// are offline (they work from the binary); only `operator run` needs the
    /// `operator` build feature (#1328).
    K8s(K8sArgs),
    /// Render tape's runtime image Dockerfiles — offline, no server. Image
    /// construction is owned here (not by `k8s`) because the same artifact
    /// feeds compose, kind, and real registries (#1328).
    Dockerfile(DockerfileArgs),
    /// Write a consistent snapshot of a RUNNING node's journal to a backup
    /// destination through the shared core/crates/service-backup runner (#1329):
    /// fetches `GET /admin/backup` and ships the bytes to `--dest`
    /// (`file://`, `s3://`, or `gs://`, workload-identity ADC in-cluster).
    /// Needs a build with `--features backup`.
    Backup(BackupArgs),
}

#[derive(clap::Args)]
pub(crate) struct UpgradeArgs {
    /// Report the current and latest version without modifying the binary.
    #[arg(long)]
    check: bool,
    /// Install this exact version (`0.4.3` or `tape@0.4.3`) instead of latest.
    #[arg(long = "version")]
    tag: Option<String>,
    /// Reinstall even when already on the selected version.
    #[arg(long)]
    force: bool,
    /// Skip the confirmation prompt.
    #[arg(short = 'y', long)]
    yes: bool,
}

pub(crate) const TOOL: cli_std::ToolInfo = cli_std::ToolInfo {
    project: "tape",
    repo: "faberline/tape",
    target: env!("TAPE_TARGET"),
    version: env!("CARGO_PKG_VERSION"),
    git_sha: env!("TAPE_GIT_SHA"),
    built_at: env!("TAPE_BUILT_AT"),
};

/// Run the parsed command.
pub(crate) async fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Append(args) => crate::offline::append(args),
        Command::Replay(args) => crate::offline::replay(args),
        Command::Checkpoint(args) => crate::offline::checkpoint(args),
        Command::Subscription(args) => crate::offline::subscription(args),
        Command::Serve(args) => crate::serve::serve_main(args).await,
        Command::Spec(args) => crate::spec::spec(args),
        Command::Llm(args) => crate::llm::llm(args),
        Command::Upgrade(args) => {
            cli_std::upgrade::run(
                &TOOL,
                cli_std::upgrade::Options {
                    check: args.check,
                    tag: args.tag,
                    force: args.force,
                    yes: args.yes,
                },
            )
            .await
        }
        Command::Issue(args) => crate::issue::issue(args).await,
        Command::K8s(args) => crate::k8s::k8s(args).await,
        Command::Dockerfile(args) => crate::dockerfile::dockerfile(args),
        Command::Backup(args) => crate::backup::dispatch_backup(args).await,
    }
}

/// Write `body` to `out` (a file, or `default_file` inside a directory) or
/// print it to stdout.
pub(crate) fn write_or_print(out: Option<&Path>, default_file: &str, body: &str) -> Result<()> {
    cli_std::artifact::write_or_print(out, default_file, body)?;
    Ok(())
}
