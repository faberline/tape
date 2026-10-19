//! Suite `image`: the image under test, started with `docker` on the runner
//! (rust-tests.md rule 6), for the architecture the shared GKE cluster does
//! not run. The runner's architecture picks the child of the image index:
//! GitHub's arm64 runner tests the linux/arm64 child. The child is the
//! expected digest and architecture, `tape --version` is the expected
//! version, the server reports the expected git sha, and an appended event
//! survives a container restart on the same data directory.
//!
//! Input: `E2E_STANDALONE_IMAGE`, the image index, plus the `E2E_EXPECTED_*`
//! identity, where `E2E_EXPECTED_RUNTIME_DIGEST` is the runner's child.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};

use super::{
    append, assert_replay, assert_version, evidence, required_env, wait_healthy, Expected,
};

const SUITE: &str = "image";
const NAME: &str = "tape-e2e-image";
const TOPIC: &str = "orders.image";
const PORT: &str = "7137/tcp";
const START_TIMEOUT: Duration = Duration::from_secs(2 * 60);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "e2e: needs E2E_STANDALONE_IMAGE"]
async fn the_image_serves_and_keeps_its_data_across_a_restart() {
    let image = required_env("E2E_STANDALONE_IMAGE");
    let expected = Expected::from_env();
    assert_eq!(
        image, expected.image,
        "E2E_STANDALONE_IMAGE must be the expected image"
    );
    let arch = docker_arch();

    // The index has exactly one child for this runner, and it is the
    // expected digest.
    let index: Value = serde_json::from_str(&docker(&[
        "buildx",
        "imagetools",
        "inspect",
        "--raw",
        &image,
    ]))
    .unwrap_or_else(|err| panic!("{image}: index is not JSON: {err}"));
    let children: Vec<&str> = index["manifests"]
        .as_array()
        .map(|all| {
            all.iter()
                .filter(|m| m["platform"]["os"] == "linux" && m["platform"]["architecture"] == arch)
                .filter_map(|m| m["digest"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        children,
        [expected.runtime_digest.as_str()],
        "{image}: linux/{arch} children"
    );

    let child = format!("ghcr.io/faberline/tape@{}", expected.runtime_digest);
    docker(&["pull", "--quiet", &child]);
    let inspect: Value = serde_json::from_str(&docker(&["image", "inspect", &child]))
        .unwrap_or_else(|err| panic!("docker image inspect {child}: not JSON: {err}"));
    assert_eq!(inspect[0]["Os"], "linux", "{child} os");
    assert_eq!(inspect[0]["Architecture"], arch, "{child} architecture");
    let repo_digests = inspect[0]["RepoDigests"].clone();
    assert!(
        repo_digests
            .as_array()
            .is_some_and(|all| all.iter().any(|d| d == child.as_str())),
        "{child} RepoDigests: {repo_digests}"
    );
    evidence(
        SUITE,
        "runner child is the expected digest",
        json!({ "image": image, "arch": arch, "child": child, "repo_digests": repo_digests }),
    );

    let version = docker(&["run", "--rm", &child, "--version"]);
    assert_eq!(
        version.trim(),
        format!("tape {}", expected.version),
        "{child} --version"
    );
    evidence(SUITE, "cli version", json!({ "version": version.trim() }));

    let data = tempfile::tempdir().expect("data directory");
    // The image runs as uid 65532, which owns nothing on the runner.
    std::fs::set_permissions(
        data.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o777),
    )
    .expect("open the data directory to the container user");
    let server = Container::start(&child, data.path());
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();

    let base = server.base();
    wait_healthy(&http, &base, START_TIMEOUT).await;
    let reported = assert_version(&http, &base, &expected).await;
    evidence(
        SUITE,
        "server reports the expected build",
        json!({ "git_sha": expected.git_sha, "version": reported }),
    );
    append(&http, &base, TOPIC, "before-restart").await;
    assert_replay(&http, &base, TOPIC, &["before-restart"]).await;
    evidence(
        SUITE,
        "append and replay",
        json!({ "topic": TOPIC, "ids": ["before-restart"] }),
    );

    docker(&["restart", NAME]);
    // Docker may publish a new host port after the restart.
    let base = server.base();
    wait_healthy(&http, &base, START_TIMEOUT).await;
    assert_version(&http, &base, &expected).await;
    assert_replay(&http, &base, TOPIC, &["before-restart"]).await;
    append(&http, &base, TOPIC, "after-restart").await;
    assert_replay(&http, &base, TOPIC, &["before-restart", "after-restart"]).await;
    evidence(
        SUITE,
        "ordered replay across restart",
        json!({ "topic": TOPIC, "ids": ["before-restart", "after-restart"] }),
    );
}

/// The server container; dropping it removes it.
struct Container;

impl Container {
    fn start(image: &str, data: &Path) -> Self {
        // A container left by an earlier run would hold the name.
        let _ = Command::new("docker").args(["rm", "-f", NAME]).output();
        let volume = format!("{}:/data", data.display());
        docker(&[
            "run",
            "--detach",
            "--name",
            NAME,
            "--publish",
            "127.0.0.1::7137",
            "--volume",
            &volume,
            "--env",
            "TAPE_BIND=0.0.0.0:7137",
            "--env",
            "TAPE_DATA_DIR=/data",
            "--env",
            "TAPE_AUTH=off",
            image,
            "serve",
        ]);
        Self
    }

    /// `http://127.0.0.1:<host port>` of the running container.
    fn base(&self) -> String {
        let port = docker(&["port", NAME, PORT]);
        let address = port
            .lines()
            .find(|line| line.starts_with("127.0.0.1:"))
            .unwrap_or_else(|| {
                panic!("docker port {NAME} {PORT}: no 127.0.0.1 address in {port:?}")
            });
        format!("http://{address}")
    }
}

impl Drop for Container {
    fn drop(&mut self) {
        if let Ok(logs) = Command::new("docker")
            .args(["logs", "--tail", "50", NAME])
            .output()
        {
            eprintln!(
                "docker logs {NAME}:\n{}{}",
                String::from_utf8_lossy(&logs.stdout),
                String::from_utf8_lossy(&logs.stderr)
            );
        }
        let _ = Command::new("docker").args(["rm", "-f", NAME]).output();
    }
}

/// The runner's architecture as an OCI platform names it.
fn docker_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => panic!("no OCI architecture for {other}"),
    }
}

/// Runs `docker <args>` and returns its stdout; a failure fails the test.
fn docker(args: &[&str]) -> String {
    let output = Command::new("docker")
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("cannot start docker: {err}"));
    assert!(
        output.status.success(),
        "docker {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn the_runner_has_an_oci_architecture() {
    assert!(matches!(docker_arch(), "amd64" | "arm64"));
}
