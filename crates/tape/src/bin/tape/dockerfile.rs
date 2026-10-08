//! `tape dockerfile`: render the runtime image Dockerfiles, offline.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Subcommand, ValueEnum};

use crate::cli::write_or_print;

/// `tape dockerfile <render>` — render tape's runtime image Dockerfiles.
#[derive(clap::Args, Debug)]
pub(crate) struct DockerfileArgs {
    #[command(subcommand)]
    pub(crate) cmd: DockerfileCmd,
}

#[derive(Subcommand, Debug)]
pub(crate) enum DockerfileCmd {
    /// Render a Dockerfile to stdout or `--out`.
    Render(DockerfileRenderArgs),
}

#[derive(clap::Args, Debug)]
pub(crate) struct DockerfileRenderArgs {
    /// Which runtime image contract to render.
    #[arg(long, value_enum, default_value_t = DockerfileVariant::Source)]
    pub(crate) variant: DockerfileVariant,
    /// Release tag used by `--variant release`; accepts `0.1.0` or
    /// `tape@0.1.0`.
    #[arg(long)]
    pub(crate) version: Option<String>,
    /// Write to this path instead of stdout. A directory receives `Dockerfile`
    /// or `Dockerfile.release`.
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum DockerfileVariant {
    /// Build from the workspace source tree.
    Source,
    /// Fetch and verify a published `tape@<version>` release binary.
    Release,
}

/// `tape dockerfile render` — render tape's runtime image Dockerfiles. The
/// checked-in Dockerfiles are the fixtures; the CLI is their in-binary form
/// (marker stripping + `tape@version` substitution), so `render` stays the
/// source of truth (relay #1208 pattern).
pub(crate) fn dockerfile(args: DockerfileArgs) -> Result<()> {
    match args.cmd {
        DockerfileCmd::Render(a) => {
            let (file_name, body) = match a.variant {
                DockerfileVariant::Source => ("Dockerfile", render_source_dockerfile()),
                DockerfileVariant::Release => (
                    "Dockerfile.release",
                    render_release_dockerfile(a.version.as_deref()),
                ),
            };
            write_or_print(a.out.as_deref(), file_name, &body)
        }
    }
}

pub(crate) fn render_source_dockerfile() -> String {
    cli_std::artifact::strip_source_ownership_markers(include_str!("../../../../../Dockerfile"))
}

pub(crate) fn render_release_dockerfile(version: Option<&str>) -> String {
    let tag = cli_std::artifact::release_tag("tape", version, env!("CARGO_PKG_VERSION"));
    let version = tag.trim_start_matches("tape@");
    let template = cli_std::artifact::strip_source_ownership_markers(include_str!(
        "../../../../../Dockerfile.release"
    ));
    let mut out = String::new();
    for line in template.lines() {
        if line.starts_with("#   docker build -f Dockerfile.release -t tape:") {
            out.push_str(&format!(
                "#   docker build -f Dockerfile.release -t tape:{version} \\"
            ));
        } else if line.starts_with("#     --build-arg TAPE_VERSION=") {
            out.push_str(&format!("#     --build-arg TAPE_VERSION={tag} ."));
        } else if line.starts_with("ARG TAPE_VERSION=") {
            out.push_str(&format!("ARG TAPE_VERSION={tag}"));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}
