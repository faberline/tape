//! End-to-end suites (faberline/aw docs/standards/rust-tests.md): each module
//! is one suite that talks to a tape running somewhere else, at
//! `E2E_ENDPOINT`. Every test is `#[ignore]`, so plain `cargo test` needs no
//! cluster; a suite runs with `--ignored --test-threads=1 e2e::<suite>::`.
//!
//! On Faberline's shared GKE test cluster the test Job in
//! `k8s/gke-test-job/<suite>/` runs this binary next to the deploy of
//! `k8s/overlays/gke-test` (`.github/workflows/gke-test.yml`). Cluster actions
//! are `kubectl` calls in the Job's namespace; every check is here in Rust,
//! and each step prints one `E2E_EVIDENCE {json}` line to stdout.
//!
//! The suite `image` is the one exception (rust-tests.md rule 6): it starts
//! the image under test with `docker` on the runner, from
//! `E2E_STANDALONE_IMAGE`, for the architecture GKE does not run.

mod image;
mod operator_ha;
mod operator_recovery;

use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Base URL of the tape under test, without a trailing slash. A missing
/// endpoint fails the test: an e2e test never passes or skips in silence.
pub(crate) fn endpoint() -> String {
    let endpoint = required_env("E2E_ENDPOINT");
    endpoint.trim_end_matches('/').to_owned()
}

/// A variable the run must set; a missing or empty one fails the test.
pub(crate) fn required_env(name: &str) -> String {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => value.trim().to_owned(),
        _ => panic!("{name} is not set; the e2e suites run from the GKE test Job, which sets it"),
    }
}

/// Prints one evidence line. `detail` is merged into the line's object.
pub(crate) fn evidence(suite: &str, step: &str, detail: Value) {
    let mut line = json!({ "suite": suite, "step": step, "ok": true });
    if let (Some(line), Value::Object(detail)) = (line.as_object_mut(), detail) {
        line.extend(detail);
    }
    println!("E2E_EVIDENCE {line}");
}

/// Runs `kubectl -n $NAMESPACE <args>` and returns its stdout, or its stderr
/// as the error.
pub(crate) fn try_kubectl(args: &[&str]) -> Result<String, String> {
    let namespace = required_env("NAMESPACE");
    let output = Command::new("kubectl")
        .arg("-n")
        .arg(&namespace)
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("cannot start kubectl: {err}"));
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

/// [`try_kubectl`] that fails the test on a kubectl error.
pub(crate) fn kubectl(args: &[&str]) -> String {
    try_kubectl(args).unwrap_or_else(|err| panic!("kubectl {}: {err}", args.join(" ")))
}

/// `kubectl get ... -o json`, parsed.
pub(crate) fn kubectl_json(args: &[&str]) -> Value {
    let mut args = args.to_vec();
    args.extend(["-o", "json"]);
    let out = kubectl(&args);
    serde_json::from_str(&out)
        .unwrap_or_else(|err| panic!("kubectl {}: not JSON: {err}", args.join(" ")))
}

/// Polls `probe` every 2 seconds until it returns `Some`, and fails the test
/// with `what` after `timeout`. `probe` also returns the last state it saw,
/// for the failure message.
pub(crate) fn wait_for<T>(
    what: &str,
    timeout: Duration,
    mut probe: impl FnMut() -> Result<T, String>,
) -> T {
    let deadline = Instant::now() + timeout;
    loop {
        match probe() {
            Ok(value) => return value,
            Err(last) if Instant::now() >= deadline => {
                panic!("{what}: not reached within {timeout:?}; last saw: {last}")
            }
            Err(_) => std::thread::sleep(Duration::from_secs(2)),
        }
    }
}

/// The identity the run expects, from the ConfigMap `gke-test-env`.
pub(crate) struct Expected {
    /// Version `tape --version` reports, for example `0.6.0`.
    pub(crate) version: String,
    /// The commit the image was built from: 8 to 40 lowercase hex.
    pub(crate) git_sha: String,
    /// `ghcr.io/faberline/tape@sha256:<root index digest>`.
    pub(crate) image: String,
    /// `sha256:` digest of the image index itself.
    pub(crate) root_digest: String,
    /// `sha256:` digest of the linux/amd64 child of the index.
    pub(crate) runtime_digest: String,
}

impl Expected {
    pub(crate) fn from_env() -> Self {
        let version = required_env("E2E_EXPECTED_VERSION");
        let git_sha = required_env("E2E_EXPECTED_GIT_SHA");
        let image = required_env("E2E_EXPECTED_IMAGE");
        let runtime_digest = required_env("E2E_EXPECTED_RUNTIME_DIGEST");
        assert!(
            is_sha(&git_sha),
            "E2E_EXPECTED_GIT_SHA must be 8 to 40 lowercase hex, got {git_sha}"
        );
        let root_digest = image
            .strip_prefix("ghcr.io/faberline/tape@")
            .filter(|digest| is_digest(digest))
            .unwrap_or_else(|| {
                panic!("E2E_EXPECTED_IMAGE must be ghcr.io/faberline/tape@sha256:<64 hex>, got {image}")
            })
            .to_owned();
        assert!(
            is_digest(&runtime_digest),
            "E2E_EXPECTED_RUNTIME_DIGEST must be sha256:<64 hex>, got {runtime_digest}"
        );
        assert_ne!(
            runtime_digest, root_digest,
            "the runtime child digest must differ from the root index digest"
        );
        Self {
            version,
            git_sha,
            image,
            root_digest,
            runtime_digest,
        }
    }

    /// Whether a container status `imageID` binds this image: its digest is
    /// the root index or the platform child.
    pub(crate) fn binds(&self, image_id: &str) -> bool {
        image_digest(image_id)
            .is_some_and(|digest| digest == self.root_digest || digest == self.runtime_digest)
    }
}

/// Polls `{base}/healthz` every 2 seconds until it answers 2xx, and fails
/// the test after `timeout`.
pub(crate) async fn wait_healthy(http: &reqwest::Client, base: &str, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let last = match http.get(format!("{base}/healthz")).send().await {
            Ok(response) if response.status().is_success() => return,
            Ok(response) => response.status().to_string(),
            Err(err) => err.to_string(),
        };
        assert!(
            tokio::time::Instant::now() < deadline,
            "{base}/healthz not healthy within {timeout:?}; last saw: {last}"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Reads `{base}/version` and checks that the running server is the build
/// under test: its version is the expected one, and its stamped git sha is
/// the expected commit. Returns the response for the evidence line.
pub(crate) async fn assert_version(
    http: &reqwest::Client,
    base: &str,
    expected: &Expected,
) -> Value {
    let response = http
        .get(format!("{base}/version"))
        .send()
        .await
        .unwrap_or_else(|err| panic!("GET {base}/version: {err}"));
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .unwrap_or_else(|err| panic!("GET {base}/version: {err}"));
    assert!(status.is_success(), "GET {base}/version: {status} {body}");
    assert_eq!(
        body["version"],
        expected.version.as_str(),
        "{base}/version version: {body}"
    );
    let git_sha = body["git_sha"].as_str().unwrap_or_default();
    assert!(
        stamps(git_sha, &expected.git_sha),
        "{base}/version git_sha {git_sha:?} is not the commit {}: {body}",
        expected.git_sha
    );
    body
}

/// Whether a stamped `git_sha` names the commit `want`. Either side is the
/// short sha or the full one (`TAPE_SOURCE_REVISION`), so one is a prefix of
/// the other.
fn stamps(git_sha: &str, want: &str) -> bool {
    is_sha(git_sha) && is_sha(want) && (git_sha.starts_with(want) || want.starts_with(git_sha))
}

/// 8 to 40 lowercase hex: a short or a full git sha.
fn is_sha(value: &str) -> bool {
    (8..=40).contains(&value.len()) && is_lower_hex(value)
}

/// Appends `{"id": id}` to `topic` and checks the echoed payload.
pub(crate) async fn append(http: &reqwest::Client, base: &str, topic: &str, id: &str) {
    let response = http
        .post(format!("{base}/topics/{topic}/append"))
        .json(&json!({ "payload": { "id": id } }))
        .send()
        .await
        .unwrap_or_else(|err| panic!("append {id}: {err}"));
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .unwrap_or_else(|err| panic!("append {id}: {err}"));
    assert!(status.is_success(), "append {id}: {status} {body}");
    assert_eq!(
        body["payload"]["id"], id,
        "append {id} echoes the payload: {body}"
    );
}

/// Replays `topic` from the start and checks the payload ids, in order.
pub(crate) async fn assert_replay(http: &reqwest::Client, base: &str, topic: &str, want: &[&str]) {
    let response = http
        .get(format!("{base}/topics/{topic}/replay"))
        .send()
        .await
        .unwrap_or_else(|err| panic!("replay: {err}"));
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .unwrap_or_else(|err| panic!("replay: {err}"));
    assert!(status.is_success(), "replay: {status} {body}");
    let ids: Vec<&str> = body["events"]
        .as_array()
        .map(|all| {
            all.iter()
                .filter_map(|e| e["payload"]["id"].as_str())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(ids, want, "replay of {topic}: {body}");
}

/// Checks that exactly `want` live pods match `label`, that each runs the
/// expected image in `name`, and that each runtime `imageID` binds it.
/// Returns the image IDs.
pub(crate) fn assert_pods_run(
    expected: &Expected,
    label: &str,
    name: &str,
    want: u64,
) -> Vec<String> {
    let pods = kubectl_json(&["get", "pods", "-l", label]);
    let live: Vec<&Value> = pods["items"]
        .as_array()
        .map(|all| {
            all.iter()
                .filter(|p| p["metadata"]["deletionTimestamp"].is_null())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(live.len() as u64, want, "pods with {label}");
    live.iter()
        .map(|pod| {
            let pod_name = pod["metadata"]["name"].as_str().unwrap_or_default();
            assert_eq!(
                container(&pod["spec"], name)["image"],
                expected.image.as_str(),
                "pod {pod_name} container {name} image"
            );
            let image_id = pod["status"]["containerStatuses"]
                .as_array()
                .and_then(|all| all.iter().find(|s| s["name"] == name))
                .and_then(|s| s["imageID"].as_str())
                .unwrap_or_default()
                .to_owned();
            assert!(
                expected.binds(&image_id),
                "pod {pod_name} container {name} runs imageID {image_id:?}, not {} or {}",
                expected.root_digest,
                expected.runtime_digest
            );
            image_id
        })
        .collect()
}

pub(crate) fn container<'a>(pod_spec: &'a Value, name: &str) -> &'a Value {
    pod_spec["containers"]
        .as_array()
        .and_then(|all| all.iter().find(|c| c["name"] == name))
        .unwrap_or_else(|| panic!("no container {name} in {pod_spec}"))
}

/// The `sha256:` digest of a container status `imageID`, in any of the forms
/// a runtime reports: `<repo>@sha256:…`, `docker-pullable://<repo>@sha256:…`,
/// or `containerd://sha256:…`.
pub(crate) fn image_digest(image_id: &str) -> Option<&str> {
    let digest = match image_id.rsplit_once('@') {
        Some((_, digest)) => digest,
        None => image_id
            .rsplit_once("://")
            .map_or(image_id, |(_, rest)| rest),
    };
    is_digest(digest).then_some(digest)
}

fn is_digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && is_lower_hex(hex))
}

fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[test]
fn image_digest_reads_every_runtime_form() {
    let digest = format!("sha256:{}", "a".repeat(64));
    for image_id in [
        format!("ghcr.io/faberline/tape@{digest}"),
        format!("docker-pullable://ghcr.io/faberline/tape@{digest}"),
        format!("containerd://{digest}"),
        digest.clone(),
    ] {
        assert_eq!(image_digest(&image_id), Some(digest.as_str()), "{image_id}");
    }
    assert_eq!(image_digest("ghcr.io/faberline/tape:0.6.0"), None);
    assert_eq!(image_digest("sha256:ABC"), None);
}

#[test]
fn stamps_accepts_the_short_and_the_full_sha() {
    let full = "0123456789abcdef0123456789abcdef01234567";
    assert!(stamps(&full[..8], "01234567"));
    assert!(stamps(full, "01234567"));
    assert!(stamps(&full[..8], full));
    assert!(stamps(full, full));
    assert!(!stamps("unknown", "01234567"));
    assert!(!stamps("0123456", "0123456"));
    assert!(!stamps("89abcdef", "01234567"));
    assert!(!stamps("0123456789ABCDEF", "01234567"));
    assert!(!stamps("89abcdef", full));
}
