use clap::Parser;

use crate::cli::{Cli, Command};

fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
    Cli::try_parse_from(std::iter::once("tape").chain(args.iter().copied()))
}

fn connect_args(args: &[&str]) -> super::ConnectArgs {
    match parse(args).expect("parse").command {
        Command::Connect(args) => args,
        _ => panic!("not a connect command"),
    }
}

#[test]
fn namespace_is_required() {
    assert!(parse(&["connect", "--service", "tape"]).is_err());
}

#[test]
fn a_service_or_a_cr_is_required() {
    assert!(parse(&["connect", "--namespace", "ns"]).is_err());
}

#[test]
fn the_cr_name_is_the_default_service() {
    let args = connect_args(&["connect", "--namespace", "ns", "--cr", "journal"]);
    assert_eq!(args.target_service().unwrap(), "journal");
    assert_eq!(args.remote_port, 7137);
    assert!(args.command.is_empty());
}

#[test]
fn an_explicit_service_wins_over_the_cr() {
    let args = connect_args(&[
        "connect",
        "--namespace",
        "ns",
        "--cr",
        "journal",
        "--service",
        "journal-client",
    ]);
    assert_eq!(args.target_service().unwrap(), "journal-client");
}

#[test]
fn port_forward_args_without_a_context() {
    let args = connect_args(&["connect", "--namespace", "ns", "--service", "tape"]);
    assert_eq!(
        args.port_forward_args(40000).unwrap(),
        ["port-forward", "-n", "ns", "svc/tape", "40000:7137"]
    );
}

#[test]
fn port_forward_args_with_a_context_and_ports() {
    let args = connect_args(&[
        "connect",
        "--context",
        "kind-tape",
        "--namespace",
        "ns",
        "--service",
        "tape",
        "--remote-port",
        "8080",
        "--",
        "curl",
        "-s",
        "$TAPE_URL/healthz",
    ]);
    assert_eq!(
        args.port_forward_args(40000).unwrap(),
        [
            "--context",
            "kind-tape",
            "port-forward",
            "-n",
            "ns",
            "svc/tape",
            "40000:8080"
        ]
    );
    assert_eq!(args.command, ["curl", "-s", "$TAPE_URL/healthz"]);
}
