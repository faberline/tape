//! `tape spec`: print the API contract and generate typed clients, offline.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Subcommand, ValueEnum};
use tape_journal::interfaces::spec;

/// `tape spec [--format ...]` or `tape spec gen ...`. Positional slots are
/// reserved for the `gen` subcommand; everything else is a flag (the CLI
/// convention).
#[derive(clap::Args)]
pub(crate) struct SpecArgs {
    /// Generate a typed client from the spec instead of printing it.
    #[command(subcommand)]
    pub(crate) gen: Option<SpecSub>,
    /// Contract format to print.
    #[arg(long, value_enum, default_value_t = SpecFormat::Openapi)]
    pub(crate) format: SpecFormat,
}

#[derive(Subcommand)]
pub(crate) enum SpecSub {
    /// Generate a typed API client (TypeScript / Python / Rust) from tape's
    /// OpenAPI document, written into `--out` (#1329).
    Gen(GenArgs),
}

#[derive(clap::Args)]
pub(crate) struct GenArgs {
    /// Target language for the generated client.
    #[arg(long, value_enum)]
    pub(crate) lang: GenLang,
    /// Pinned generated-client contract, e.g. `python-3.14`. Defaults to
    /// `clients/codegen.toml`; an explicit value overrides that policy once.
    #[arg(long, value_name = "TARGET")]
    pub(crate) target: Option<String>,
    /// Output directory for the generated files.
    #[arg(long)]
    pub(crate) out: PathBuf,
    /// HTTP backend for the TypeScript client (ignored for py/rust).
    #[arg(long, value_enum, default_value_t = GenHttp::Fetch)]
    pub(crate) http: GenHttp,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum GenLang {
    /// TypeScript: types + fetch/axios client.
    Ts,
    /// Python: pydantic models + a generated HTTP client.
    Py,
    /// Rust: serde models + a reqwest client.
    Rust,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum GenHttp {
    Fetch,
    Axios,
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum SpecFormat {
    Openapi,
    OpenapiYaml,
    JsonSchema,
    Routes,
}

pub(crate) fn spec(args: SpecArgs) -> Result<()> {
    // `spec gen` writes a typed client; everything else prints to stdout.
    if let Some(SpecSub::Gen(gen)) = args.gen {
        return spec_gen(gen);
    }
    let out = match args.format {
        SpecFormat::Openapi => spec::openapi_json(),
        SpecFormat::OpenapiYaml => spec::openapi_yaml(),
        SpecFormat::JsonSchema => spec::json_schema_json(),
        SpecFormat::Routes => spec::routes_json(),
    };
    println!("{out}");
    Ok(())
}

/// `tape spec gen` — generate a typed client from tape's own OpenAPI
/// document (offline; no server) via the shared `core/crates/openapi-codegen`,
/// written into `--out`. One codegen path, no external tool (relay #1209
/// pattern).
pub(crate) fn spec_gen(args: GenArgs) -> Result<()> {
    use openapi_codegen::{
        generate_for_target, GenOptions, HttpClient, Lang, TargetPolicy, MANIFEST_FILE,
    };

    const TARGET_POLICY: &str = include_str!("../../../../../clients/codegen.toml");

    let lang = match args.lang {
        GenLang::Ts => Lang::Ts,
        GenLang::Py => Lang::Py,
        GenLang::Rust => Lang::Rust,
    };
    let target = TargetPolicy::from_toml(TARGET_POLICY)?.resolve(lang, args.target.as_deref())?;
    let opts = GenOptions {
        lang,
        target: Some(target),
        spec_path: PathBuf::new(),
        out_dir: args.out.clone(),
        client_name: "createClient".to_string(),
        http_client: match args.http {
            GenHttp::Fetch => HttpClient::Fetch,
            GenHttp::Axios => HttpClient::Axios,
        },
        emit_types: true,
        emit_client: true,
        // TanStack Query hooks are a TypeScript-only concern.
        emit_hooks: matches!(lang, Lang::Ts),
    };
    let output = generate_for_target(&spec::openapi_json(), &opts, target)?;
    output.write_to_dir(&args.out)?;
    for file in &output.files {
        let path = args.out.join(&file.rel_path);
        println!("generated {}", path.display());
    }
    println!("generated {}", args.out.join(MANIFEST_FILE).display());
    let requirements = output.requirements.expect("explicit target requirements");
    println!(
        "target: {} (minimum {} {})",
        requirements.target,
        requirements.language.id(),
        requirements.minimum_version
    );
    let entry_file = match lang {
        Lang::Ts => "index.ts",
        Lang::Py => "__init__.py",
        Lang::Rust => "mod.rs",
    };
    println!("next: {}", args.out.join(entry_file).display());
    Ok(())
}
