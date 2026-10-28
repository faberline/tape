use super::{dispatch, Cli, Command, DurableArgs, OutputFormat};
use clap::{error::ErrorKind, Parser};

#[test]
fn local_run_preserves_workload_and_json_report_selection() {
    let cli = Cli::try_parse_from([
        "tape-bench",
        "run",
        "--events",
        "1000",
        "--payload-bytes",
        "513",
        "--format",
        "json",
    ])
    .expect("documented local benchmark command");
    let Command::Run(args) = cli.command else {
        panic!("wrong benchmark mode")
    };
    assert_eq!(args.events, 1000);
    assert_eq!(args.payload_bytes, 513);
    assert!(matches!(args.format, OutputFormat::Json));
}

#[test]
fn durable_sweep_preserves_connections_and_per_connection_workload() {
    let cli = Cli::try_parse_from([
        "tape-bench",
        "durable",
        "--connections",
        "16,1,4",
        "--events-per-connection",
        "7",
        "--payload-bytes",
        "513",
        "--format",
        "json",
    ])
    .expect("documented connection sweep");
    let Command::Durable(args) = cli.command else {
        panic!("wrong benchmark mode")
    };
    assert_eq!(args.connections, [16, 1, 4]);
    assert_eq!(args.events_per_connection, 7);
    assert_eq!(args.payload_bytes, 513);
    assert!(matches!(args.format, OutputFormat::Json));
}

#[test]
fn malformed_workloads_and_unknown_report_formats_are_rejected() {
    let format = Cli::try_parse_from(["tape-bench", "run", "--format", "yaml"])
        .err()
        .expect("unsupported format must fail");
    assert_eq!(format.kind(), ErrorKind::InvalidValue);
    assert!(Cli::try_parse_from(["tape-bench", "run", "--events", "-1"]).is_err());
    assert!(
        Cli::try_parse_from(["tape-bench", "durable", "--connections", "1,invalid,16"]).is_err()
    );
}

#[test]
fn empty_connection_sweep_refuses_before_running_a_benchmark() {
    let error = dispatch(Cli {
        command: Command::Durable(DurableArgs {
            events_per_connection: 7,
            payload_bytes: 513,
            connections: Vec::new(),
            format: OutputFormat::Text,
        }),
    })
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "--connections requires at least one connection count"
    );
}

#[cfg(not(feature = "jetstream-benchmark"))]
#[test]
fn jetstream_commands_require_the_benchmark_feature() {
    for command in ["jetstream-client", "jetstream-suite"] {
        let error = Cli::try_parse_from(["tape-bench", command, "--help"])
            .err()
            .expect("feature-disabled command must fail");
        assert_eq!(error.kind(), ErrorKind::InvalidSubcommand);
    }
}

#[cfg(feature = "jetstream-benchmark")]
#[test]
fn jetstream_feature_exposes_both_command_contracts_without_network_io() {
    for command in ["jetstream-client", "jetstream-suite"] {
        let help = Cli::try_parse_from(["tape-bench", command, "--help"])
            .err()
            .expect("help is returned without dispatch");
        assert_eq!(help.kind(), ErrorKind::DisplayHelp);
        assert!(help.to_string().contains(command));
    }
}
