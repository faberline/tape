//! `tape backup`: snapshot a running node to a backup destination.

use anyhow::{Context, Result};

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
    /// The file form is `--token-file`.
    #[arg(long, env = "TAPE_BACKUP_TOKEN")]
    pub(crate) token: Option<String>,
    /// File whose trimmed content is the bearer token (for example a
    /// CSI-projected Secret). Mutually exclusive with `--token`.
    #[arg(long, env = "TAPE_BACKUP_TOKEN_FILE", conflicts_with = "token")]
    pub(crate) token_file: Option<std::path::PathBuf>,
    /// Drop backup objects older than this many seconds after a successful
    /// put. Omit to keep everything.
    #[arg(long)]
    pub(crate) retention_secs: Option<u64>,
}

/// Resolve the bearer token: the trimmed content of `--token-file` when set,
/// otherwise `--token`. An unreadable or empty token file is an error.
#[cfg_attr(not(feature = "backup"), allow(dead_code))]
fn resolve_token(args: &BackupArgs) -> Result<Option<String>> {
    let Some(path) = &args.token_file else {
        return Ok(args.token.clone());
    };
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("read backup token file {}", path.display()))?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        anyhow::bail!("backup token file {} is empty", path.display());
    }
    Ok(Some(trimmed.to_string()))
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
    let token = resolve_token(&args)?;
    let result = tape::backup::run_backup(&args.url, token.as_deref(), &dest, &retention).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args(token: Option<&str>, token_file: Option<std::path::PathBuf>) -> BackupArgs {
        BackupArgs {
            url: "u".into(),
            dest: "d".into(),
            token: token.map(str::to_string),
            token_file,
            retention_secs: None,
        }
    }

    #[test]
    fn token_file_is_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        std::fs::write(&path, "s3cret\n").unwrap();
        let got = resolve_token(&args(None, Some(path))).unwrap();
        assert_eq!(got.as_deref(), Some("s3cret"));
    }

    #[test]
    fn empty_token_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        std::fs::write(&path, "  \n").unwrap();
        let err = resolve_token(&args(None, Some(path.clone()))).unwrap_err();
        assert!(err.to_string().contains(&path.display().to_string()));
    }

    #[test]
    fn missing_token_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist");
        let err = resolve_token(&args(None, Some(path.clone()))).unwrap_err();
        assert!(err.to_string().contains(&path.display().to_string()));
    }

    #[test]
    fn no_token_file_uses_token_arg() {
        let got = resolve_token(&args(Some("abc"), None)).unwrap();
        assert_eq!(got.as_deref(), Some("abc"));
        let none = resolve_token(&args(None, None)).unwrap();
        assert!(none.is_none());
    }
}
