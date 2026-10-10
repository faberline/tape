//! CLI surface tests: every verb parses, and the offline renders and
//! generators produce what the deploy fixtures expect.

use std::path::PathBuf;

use clap::CommandFactory;
use tape_journal::interfaces::spec as tape_spec;

use super::*;
use cli::{Cli, Command};
use dockerfile::{DockerfileArgs, DockerfileCmd, DockerfileVariant};
use k8s::{
    K8sArgs, K8sCmd, K8sCrdArgs, K8sCrdCmd, K8sInstanceArgs, K8sInstanceCmd, K8sInstanceProfile,
    K8sOperatorArgs,
};
use serve::{resolve_journal_store, JournalStoreKind};
use spec::{spec_gen, GenArgs, GenHttp, GenLang, SpecArgs, SpecSub};

#[test]
fn cli_parse_surface() {
    Cli::command().debug_assert();

    // #1325: `serve` gains --bind/--store/--grace-secs, with env fallback
    // and a 10s default grace window; existing commands keep parsing.
    // #1326: `serve` also gains --auth/--token-registry-file, defaulting
    // to tokenless (`off`).
    // #2484: `serve` gains --body-limit-bytes (env TAPE_BODY_LIMIT_BYTES,
    // default 8 MiB).
    let cli = Cli::try_parse_from(["tape", "serve"]).unwrap();
    let Command::Serve(args) = cli.command else {
        panic!("expected Serve");
    };
    assert_eq!(args.bind, "127.0.0.1:7137");
    assert!(args.store.is_none());
    assert_eq!(args.grace_secs, 10);
    assert_eq!(args.drain_delay_secs, 5);
    assert_eq!(args.auth, "off");
    assert!(args.token_registry_file.is_none());
    assert_eq!(args.body_limit_bytes, 8 * 1024 * 1024);

    let cli = Cli::try_parse_from([
        "tape",
        "serve",
        "--bind",
        "0.0.0.0:9000",
        "--store",
        "/tmp/journal.json",
        "--grace-secs",
        "3",
        "--drain-delay-secs",
        "1",
        "--auth",
        "required",
        "--token-registry-file",
        "/tmp/tape-token-registry.json",
        "--body-limit-bytes",
        "16777216",
    ])
    .unwrap();
    let Command::Serve(args) = cli.command else {
        panic!("expected Serve");
    };
    assert_eq!(args.bind, "0.0.0.0:9000");
    assert_eq!(args.store, Some(PathBuf::from("/tmp/journal.json")));
    assert_eq!(args.grace_secs, 3);
    assert_eq!(args.drain_delay_secs, 1);
    assert_eq!(args.auth, "required");
    assert_eq!(
        args.token_registry_file,
        Some(PathBuf::from("/tmp/tape-token-registry.json"))
    );
    assert_eq!(args.body_limit_bytes, 16777216);

    let cli = Cli::try_parse_from(["tape", "append", "orders", "--payload", "{\"n\":1}"]).unwrap();
    assert!(matches!(cli.command, Command::Append(_)));
}

#[test]
fn single_node_data_dir_resolves_to_the_wal_journal_store() {
    let data_dir = PathBuf::from("/data");
    assert_eq!(
        resolve_journal_store(None, Some(&data_dir), false),
        JournalStoreKind::Wal(PathBuf::from("/data")),
        "#3052: --data-dir alone now resolves to the WAL group-commit store, not the \
         old whole-file journal.json"
    );
    assert_eq!(
        resolve_journal_store(
            Some(PathBuf::from("/tmp/override.json")),
            Some(&data_dir),
            false,
        ),
        JournalStoreKind::LegacyFile(PathBuf::from("/tmp/override.json")),
        "an explicit --store must keep precedence over --data-dir, and stays on the \
         unchanged legacy whole-file path"
    );
    assert_eq!(
        resolve_journal_store(None, Some(&data_dir), true),
        JournalStoreKind::None,
        "replica mode persists through Raft rather than a parallel local store"
    );
    assert_eq!(
        resolve_journal_store(None, None, false),
        JournalStoreKind::None,
        "no --store and no --data-dir means no local durable store"
    );
}

/// #1328: `tape k8s <crd|operator|instance>` parses with the expected
/// subcommands and flags.
#[test]
fn k8s_verbs_parse() {
    let cli = Cli::try_parse_from(["tape", "k8s", "crd", "render"]).expect("crd render");
    assert!(matches!(
        cli.command,
        Command::K8s(K8sArgs {
            cmd: K8sCmd::Crd(K8sCrdArgs {
                cmd: K8sCrdCmd::Render(_),
            }),
        })
    ));

    let cli = Cli::try_parse_from([
        "tape",
        "k8s",
        "instance",
        "render",
        "--profile",
        "prod",
        "--namespace",
        "production",
    ])
    .expect("instance render");
    match cli.command {
        Command::K8s(K8sArgs {
            cmd:
                K8sCmd::Instance(K8sInstanceArgs {
                    cmd: K8sInstanceCmd::Render(a),
                }),
        }) => {
            assert!(matches!(a.profile, K8sInstanceProfile::Prod));
            assert_eq!(a.namespace.as_deref(), Some("production"));
        }
        _ => panic!("expected k8s instance render"),
    }

    // `operator` with no subcommand defaults to `run`.
    let cli = Cli::try_parse_from(["tape", "k8s", "operator"]).expect("operator default");
    match cli.command {
        Command::K8s(K8sArgs {
            cmd: K8sCmd::Operator(K8sOperatorArgs { cmd }),
        }) => assert!(cmd.is_none()),
        _ => panic!("expected k8s operator"),
    }
}

/// #1329: `tape backup` parses its flags (url/dest/token/retention-secs).
#[test]
fn backup_verb_parses() {
    let cli = Cli::try_parse_from([
        "tape",
        "backup",
        "--url",
        "http://localhost:7137",
        "--dest",
        "file:///tmp/backups",
        "--token",
        "s3cr3t",
        "--retention-secs",
        "3600",
    ])
    .expect("backup should parse");
    match cli.command {
        Command::Backup(a) => {
            assert_eq!(a.url, "http://localhost:7137");
            assert_eq!(a.dest, "file:///tmp/backups");
            assert_eq!(a.token.as_deref(), Some("s3cr3t"));
            assert_eq!(a.retention_secs, Some(3600));
        }
        _ => panic!("expected backup"),
    }
}

/// #1329: `tape spec gen` parses its flags and generates non-empty client
/// output for each supported language from tape's own OpenAPI document.
#[test]
fn spec_gen_verbs_parse_and_generate() {
    let cli = Cli::try_parse_from([
        "tape",
        "spec",
        "gen",
        "--lang",
        "ts",
        "--target",
        "typescript-5.0",
        "--out",
        "/tmp/x",
    ])
    .expect("spec gen should parse");
    match cli.command {
        Command::Spec(SpecArgs {
            gen: Some(SpecSub::Gen(a)),
            ..
        }) => {
            assert!(matches!(a.lang, GenLang::Ts));
            assert_eq!(a.target.as_deref(), Some("typescript-5.0"));
            assert_eq!(a.out, PathBuf::from("/tmp/x"));
        }
        _ => panic!("expected spec gen"),
    }

    for lang in [GenLang::Ts, GenLang::Py, GenLang::Rust] {
        let opts = openapi_codegen::GenOptions {
            lang: match lang {
                GenLang::Ts => openapi_codegen::Lang::Ts,
                GenLang::Py => openapi_codegen::Lang::Py,
                GenLang::Rust => openapi_codegen::Lang::Rust,
            },
            target: None,
            spec_path: PathBuf::new(),
            out_dir: PathBuf::new(),
            client_name: "createClient".to_string(),
            http_client: openapi_codegen::HttpClient::Fetch,
            emit_types: true,
            emit_client: true,
            emit_hooks: matches!(lang, GenLang::Ts),
        };
        let out = openapi_codegen::generate(&tape_spec::openapi_json(), &opts)
            .expect("spec gen should succeed for tape's own OpenAPI document");
        assert!(
            !out.files.is_empty(),
            "{lang:?} should emit at least one file"
        );
    }

    let out_dir = tempfile::tempdir().expect("create generated-client directory");
    spec_gen(GenArgs {
        lang: GenLang::Py,
        target: None,
        out: out_dir.path().to_path_buf(),
        http: GenHttp::Fetch,
    })
    .expect("tape default target policy should generate");
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(out_dir.path().join(".openapi-codegen.json"))
            .expect("read generated manifest"),
    )
    .expect("parse generated manifest");
    assert_eq!(manifest["target"], "python-3.14");
    assert_eq!(manifest["minimum_version"], "3.14");
}

/// #1328: `tape dockerfile render` parses with variant/version/out flags,
/// and the shared tag helper converges bare/prefixed tags.
#[test]
fn dockerfile_verbs_parse() {
    let cli = Cli::try_parse_from([
        "tape",
        "dockerfile",
        "render",
        "--variant",
        "release",
        "--version",
        "1.2.3",
    ])
    .expect("dockerfile render should parse");
    match cli.command {
        Command::Dockerfile(DockerfileArgs {
            cmd: DockerfileCmd::Render(a),
        }) => {
            assert!(matches!(a.variant, DockerfileVariant::Release));
            assert_eq!(a.version.as_deref(), Some("1.2.3"));
        }
        _ => panic!("expected dockerfile render"),
    }
    assert_eq!(
        cli_std::artifact::release_tag("tape", Some("1.2.3"), env!("CARGO_PKG_VERSION")),
        "tape@1.2.3"
    );
    assert_eq!(
        cli_std::artifact::release_tag("tape", Some("tape@1.2.3"), env!("CARGO_PKG_VERSION")),
        "tape@1.2.3"
    );
    assert_eq!(
        cli_std::artifact::release_tag("tape", None, env!("CARGO_PKG_VERSION")),
        format!("tape@{}", env!("CARGO_PKG_VERSION"))
    );
}
