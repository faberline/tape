//! `tape connect` drives the port-forward lifecycle end to end. A stand-in
//! `kubectl` on `PATH` answers `port-forward` by serving tape itself on the
//! requested local port, so the case needs no cluster: it proves the wrapped
//! command runs only once the port is up, sees `TAPE_URL`, has its exit
//! status passed through, and that the port-forward is gone afterwards.

use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn write_script(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn connect_runs_the_command_against_the_forward_and_tears_it_down() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    // The last argument is `<local>:<remote>`; serve tape on `<local>`.
    write_script(
        &bin.join("kubectl"),
        r#"#!/bin/sh
echo "$@" > "$CONNECT_TEST_DIR/kubectl-args"
for arg in "$@"; do last=$arg; done
exec "$CONNECT_TEST_TAPE" serve --bind "127.0.0.1:${last%%:*}"
"#,
    );
    let wrapped = dir.path().join("wrapped");
    write_script(
        &wrapped,
        r#"#!/bin/sh
echo "$TAPE_URL" > "$CONNECT_TEST_DIR/tape-url"
exit 7
"#,
    );
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let status = Command::new(env!("CARGO_BIN_EXE_tape"))
        .args([
            "connect",
            "--context",
            "kind-tape",
            "--namespace",
            "apps",
            "--cr",
            "journal",
            "--",
        ])
        .arg(&wrapped)
        .env("PATH", path)
        .env("CONNECT_TEST_DIR", dir.path())
        .env("CONNECT_TEST_TAPE", env!("CARGO_BIN_EXE_tape"))
        .status()
        .expect("run tape connect");

    assert_eq!(
        status.code(),
        Some(7),
        "tape connect exits with the wrapped command's status"
    );
    let url = std::fs::read_to_string(dir.path().join("tape-url")).unwrap();
    let port: u16 = url
        .trim()
        .strip_prefix("http://127.0.0.1:")
        .unwrap_or_else(|| panic!("TAPE_URL is a loopback URL: {url}"))
        .parse()
        .unwrap();
    let kubectl_args = std::fs::read_to_string(dir.path().join("kubectl-args")).unwrap();
    assert_eq!(
        kubectl_args.trim(),
        format!("--context kind-tape port-forward -n apps svc/journal {port}:7137")
    );
    assert!(
        TcpStream::connect(("127.0.0.1", port)).is_err(),
        "the port-forward is torn down once the wrapped command exits"
    );
}
