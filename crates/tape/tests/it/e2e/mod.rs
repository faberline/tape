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
    /// First 8 hex of the commit the image was built from.
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
            git_sha.len() == 8 && is_lower_hex(&git_sha),
            "E2E_EXPECTED_GIT_SHA must be 8 lowercase hex, got {git_sha}"
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
