//! `tape backup`: snapshot a running node to a backup destination.

use anyhow::Result;

/// `tape backup` flags (#1329): pulls a snapshot over HTTP from a running
/// node and ships it to a destination via `core/crates/service-backup` (relay #1209
/// pattern).
#[derive(clap::Args)]
pub(crate) struct BackupArgs {
    /// Base URL of a running tape node, e.g.
    /// `http://<name>.<namespace>.svc.cluster.local:7137` (what the
    /// operator's backup CronJob passes) or `http://localhost:7137` for ad
    /// hoc use.
    #[arg(long)]
    pub(crate) url: String,
    /// Destination URI: `file:///path`, `s3://bucket/prefix`, or
    /// `gs://bucket/prefix`. `gs://` uses workload-identity ADC in-cluster
    /// (GKE-proven) and Vat's `STORAGE_EMULATOR_HOST` locally.
    #[arg(long)]
    pub(crate) dest: String,
    /// Bearer token for `/admin/backup` (needs `admin` on `*`). Falls back to
    /// `TAPE_BACKUP_TOKEN`; omit entirely when the node runs `--auth disabled`.
    #[arg(long, env = "TAPE_BACKUP_TOKEN")]
    pub(crate) token: Option<String>,
    /// Drop backup objects older than this many seconds after a successful
    /// put. Omit to keep everything.
    #[arg(long)]
    pub(crate) retention_secs: Option<u64>,
}

/// `tape backup` (#1329): fetch `{url}/admin/backup` and ship the bytes to
/// `--dest` via `core/crates/service-backup`, printing the resulting
/// `BackupRunResult` as JSON. This is what the operator's optional backup
/// CronJob invokes on a schedule; it works equally ad hoc.
#[cfg(feature = "backup")]
pub(crate) async fn dispatch_backup(args: BackupArgs) -> Result<()> {
    let dest = service_backup::BackupDestination::from_uri(&args.dest)?;
    let retention = match args.retention_secs {
        Some(secs) => service_backup::RetentionPolicy::max_age_seconds(secs),
        None => service_backup::RetentionPolicy::default(),
    };
    let result =
        tape::backup::run_backup(&args.url, args.token.as_deref(), &dest, &retention).await?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

#[cfg(not(feature = "backup"))]
pub(crate) async fn dispatch_backup(_args: BackupArgs) -> Result<()> {
    anyhow::bail!(
        "this tape build was compiled without backup support; rebuild with \
         `--features backup` (the published image includes it)"
    )
}
