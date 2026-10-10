//! `tape connect`: port-forward to a tape Service in a cluster and run a
//! command against it.

use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result};

#[cfg(test)]
mod tests;

/// How long the local end of the port-forward may take to accept a
/// connection.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// `tape connect` flags: manage a `kubectl port-forward` around a wrapped
/// command so the caller never tracks the port-forward process itself.
#[derive(clap::Args)]
pub(crate) struct ConnectArgs {
    /// kubectl context to port-forward through. Omit to use the current context.
    #[arg(long)]
    pub(crate) context: Option<String>,
    /// Namespace of the target Service.
    #[arg(long)]
    pub(crate) namespace: String,
    /// Target Service name. Defaults to the `--cr` name: the operator names a
    /// `Tape` instance's client Service after the CR.
    #[arg(long, required_unless_present = "cr")]
    pub(crate) service: Option<String>,
    /// `Tape` CR name, used as the Service name when `--service` is omitted.
    #[arg(long)]
    pub(crate) cr: Option<String>,
    /// Local port to forward to. Omit to pick a free ephemeral port.
    #[arg(long)]
    pub(crate) local_port: Option<u16>,
    /// Remote (Service) port.
    #[arg(long, default_value_t = 7137)]
    pub(crate) remote_port: u16,
    /// Command to run with `TAPE_URL` set to the forwarded address. Omit to
    /// print the address and hold the port-forward open until Ctrl-C.
    #[arg(last = true)]
    pub(crate) command: Vec<String>,
}

impl ConnectArgs {
    /// The Service to forward to: `--service`, or else the `--cr` name.
    pub(crate) fn target_service(&self) -> Result<&str> {
        self.service
            .as_deref()
            .or(self.cr.as_deref())
            .context("--service or --cr is required")
    }

    /// The `kubectl` arguments that forward `local_port` to the Service.
    pub(crate) fn port_forward_args(&self, local_port: u16) -> Result<Vec<String>> {
        let mut args = Vec::new();
        if let Some(context) = &self.context {
            args.extend(["--context".to_string(), context.clone()]);
        }
        args.extend([
            "port-forward".to_string(),
            "-n".to_string(),
            self.namespace.clone(),
            format!("svc/{}", self.target_service()?),
            format!("{local_port}:{}", self.remote_port),
        ]);
        Ok(args)
    }
}

/// `tape connect`: spawn `kubectl port-forward`, wait until the local end
/// accepts connections, then run the wrapped command with `TAPE_URL` set and
/// exit with its status. The port-forward is torn down on every path
/// (`ChildGuard::drop`).
///
/// No credential is injected: a token in the child's environment would be
/// inherited by every process it starts. Against a node with
/// `TAPE_AUTH=required`, the wrapped command passes its own bearer token.
pub(crate) async fn connect(args: ConnectArgs) -> Result<()> {
    let local_port = match args.local_port {
        Some(port) => port,
        None => cli_std::connect::free_local_port()?,
    };
    let mut forward_cmd = Command::new("kubectl");
    forward_cmd
        .args(args.port_forward_args(local_port)?)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let forward = cli_std::connect::ChildGuard::spawn(&mut forward_cmd)
        .context("start kubectl port-forward")?;
    cli_std::connect::wait_for_local_port_ready(local_port, READY_TIMEOUT)?;

    let url = format!("http://127.0.0.1:{local_port}");
    // On stderr, so the wrapped command's stdout stays machine-readable.
    eprintln!(
        "tape connect: forwarding {url} -> svc/{}:{} in {}, with no credential",
        args.target_service()?,
        args.remote_port,
        args.namespace
    );

    let Some((program, rest)) = args.command.split_first() else {
        println!("{url}");
        tokio::signal::ctrl_c()
            .await
            .context("listen for interrupt")?;
        return Ok(());
    };
    let mut child = tokio::process::Command::new(program)
        .args(rest)
        .env("TAPE_URL", &url)
        .spawn()
        .with_context(|| format!("run {program}"))?;
    let status = tokio::select! {
        status = child.wait() => status.context("wait for the wrapped command")?,
        signal = tokio::signal::ctrl_c() => {
            signal.context("listen for interrupt")?;
            let _ = child.start_kill();
            let _ = child.wait().await;
            anyhow::bail!("interrupted");
        }
    };
    drop(forward);
    if !status.success() {
        // `exit` skips destructors, so the port-forward is dropped above.
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}
