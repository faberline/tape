//! Contract checks for the Tape-owned durable-v1 benchmark runner.
//!
//! A normal Cargo test must not create a Kind cluster or contact GKE.  The
//! successful Kind command is an explicit acceptance action owned by the
//! outer runner.  This test therefore exercises only fail-closed CLI paths
//! and inspects the checked-in contract and deployment inputs.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const TAPE_IMAGE: &str =
    "ghcr.io/chrischeng-c4/tape@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn runner() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/jetstream-benchmark/run")
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("Tape manifest must be under apps/tape")
        .to_path_buf()
}

fn durable_profile() -> serde_json::Value {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(
        &fs::read_to_string(root.join("benchmarks/jetstream/durable-v1.json"))
            .expect("read durable-v1 profile"),
    )
    .expect("durable-v1 profile must be valid JSON")
}

fn pinned_nats_image() -> String {
    durable_profile()["images"]["nats_image"]
        .as_str()
        .expect("durable-v1 must pin images.nats_image")
        .to_string()
}

fn is_full_sha256_image(image: &str) -> bool {
    let Some((repository, digest)) = image.split_once("@sha256:") else {
        return false;
    };
    !repository.is_empty()
        && digest.len() == 64
        && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        && digest.bytes().all(|byte| !byte.is_ascii_uppercase())
}

fn changed_digest(image: &str) -> String {
    let (repository, digest) = image
        .split_once("@sha256:")
        .expect("test image must contain a digest");
    let mut bytes = digest.as_bytes().to_vec();
    bytes[63] = if bytes[63] == b"0"[0] {
        b"1"[0]
    } else {
        b"0"[0]
    };
    format!(
        "{repository}@sha256:{}",
        String::from_utf8(bytes).expect("hex digest")
    )
}

fn invoke(output_dir: &Path, args: &[&str]) -> Output {
    Command::new("bash")
        .arg(runner())
        .current_dir(repo_root())
        .args(["run", "--backend", "kind"])
        .args(args)
        .args(["--profile", "durable-v1", "--output"])
        .arg(output_dir)
        .output()
        .expect("start the Tape-owned benchmark runner")
}

fn common_images(nats_image: &str) -> [&str; 4] {
    ["--tape-image", TAPE_IMAGE, "--nats-image", nats_image]
}

fn record_parser() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("benchmarks/jetstream/parse_records.py")
}

fn benchmark_record(
    target: &str,
    phase: &str,
    payload_bytes: u64,
    clients: u64,
    sample_index: u64,
    run_id: String,
    topic: String,
) -> serde_json::Value {
    let is_tape = target == "tape";
    let replay = if phase == "normal" {
        serde_json::json!({
            "expected": 0,
            "received": 0,
            "loss_count": 0,
            "duplicate_count": 0,
            "error_count": 0,
            "passed": false,
        })
    } else {
        serde_json::json!({
            "expected": 100_000,
            "received": 100_000,
            "loss_count": 0,
            "duplicate_count": 0,
            "error_count": 0,
            "passed": true,
        })
    };
    serde_json::json!({
        "schema": "tape-vs-jetstream-record.v1",
        "target": target,
        "run_id": run_id,
        "topic": topic,
        "cell": {"payload_bytes": payload_bytes, "clients": clients},
        "sample_index": sample_index,
        "phase": phase,
        "throughput_ops_per_sec": if is_tape { 200.0 } else { 100.0 },
        "p99_ms": if is_tape { 1.0 } else { 2.0 },
        "replay": replay,
        "ack_batch_size": 100,
        "error_count": 0,
    })
}

fn valid_benchmark_records() -> Vec<serde_json::Value> {
    let mut records = Vec::with_capacity(126);
    for payload_bytes in [128, 1024, 4096] {
        for clients in [1, 16, 64] {
            for sample_index in 0..5 {
                for target in ["tape", "jetstream"] {
                    records.push(benchmark_record(
                        target,
                        "normal",
                        payload_bytes,
                        clients,
                        sample_index,
                        format!("run-{target}-{payload_bytes}-{clients}-{sample_index}"),
                        format!("durable-v1-{payload_bytes}-{clients}-{sample_index}"),
                    ));
                }
            }
        }
    }
    for phase in ["finalize", "recovery"] {
        for payload_bytes in [128, 1024, 4096] {
            for clients in [1, 16, 64] {
                for target in ["tape", "jetstream"] {
                    records.push(benchmark_record(
                        target,
                        phase,
                        payload_bytes,
                        clients,
                        4,
                        format!("run-{target}-{payload_bytes}-{clients}-4"),
                        format!("durable-v1-{payload_bytes}-{clients}-4"),
                    ));
                }
            }
        }
    }
    records
}

fn run_record_parser(records: &[serde_json::Value]) -> Output {
    let output_dir = tempfile::tempdir().expect("create parser fixture directory");
    let records_path = output_dir.path().join("records.jsonl");
    let mut file = fs::File::create(&records_path).expect("create parser fixture");
    for record in records {
        writeln!(
            file,
            "{}",
            serde_json::to_string(record).expect("serialize parser fixture record")
        )
        .expect("write parser fixture record");
    }
    drop(file);

    Command::new("python3")
        .arg(record_parser())
        .arg(records_path)
        .current_dir(repo_root())
        .output()
        .expect("run offline benchmark record parser")
}

fn assert_parser_rejects(records: &[serde_json::Value], expected_error: &str) {
    let output = run_record_parser(records);
    assert!(!output.status.success(), "parser must reject the mutated fixture");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(expected_error),
        "parser failure must contain {expected_error:?}; stderr was {stderr}"
    );
}

#[test]
fn invalid_image_is_rejected_before_backend_start() {
    let output_dir = tempfile::tempdir().expect("create an empty benchmark output directory");
    let nats_image = pinned_nats_image();
    let output = invoke(
        output_dir.path(),
        &[
            "--tape-image",
            "ghcr.io/chrischeng-c4/tape:latest",
            "--nats-image",
            &nats_image,
        ],
    );

    assert!(!output.status.success(), "mutable image must fail closed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("mutable image") || stderr.contains("digest"),
        "failure must explain the immutable-image requirement: {stderr}"
    );
    assert!(
        fs::read_dir(output_dir.path())
            .expect("read output directory")
            .next()
            .is_none(),
        "validation failure must not create report or backend resources"
    );
}

#[test]
fn profile_validation_fails_without_starting_kind() {
    let output_dir = tempfile::tempdir().expect("create an empty benchmark output directory");
    let nats_image = pinned_nats_image();
    let output = Command::new("bash")
        .arg(runner())
        .current_dir(repo_root())
        .args(["run", "--backend", "kind"])
        .args(common_images(&nats_image))
        .args(["--profile", "wrong-profile", "--output"])
        .arg(output_dir.path())
        .output()
        .expect("start the Tape-owned benchmark runner");

    assert!(!output.status.success(), "unknown profile must fail closed");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("durable-v1"),
        "profile rejection must name the supported profile"
    );
    assert!(
        fs::read_dir(output_dir.path())
            .expect("read output directory")
            .next()
            .is_none(),
        "profile validation failure must not create output"
    );
}

#[test]
fn gke_requires_explicit_authorization_and_records_not_run() {
    let output_dir = tempfile::tempdir().expect("create an empty benchmark output directory");
    let nats_image = pinned_nats_image();
    let output = Command::new("bash")
        .arg(runner())
        .current_dir(repo_root())
        .args(["run", "--backend", "gke"])
        .args(common_images(&nats_image))
        .args(["--profile", "durable-v1", "--output"])
        .arg(output_dir.path())
        .output()
        .expect("start the Tape-owned benchmark runner");

    assert!(!output.status.success(), "unauthorized GKE must not run");
    let refusal = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        refusal.contains("authorization"),
        "GKE refusal must explain the authorization requirement"
    );

    let report: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(output_dir.path().join("tape-vs-jetstream-report.v1.json"))
            .expect("unauthorized GKE must leave a report"),
    )
    .expect("report must be valid JSON");
    assert_eq!(report["schema"], "tape-vs-jetstream-report.v1");
    assert_eq!(report["backend"], "gke");
    assert_eq!(report["status"], "NOT_RUN");
    assert_eq!(report["verdict"], "NOT_RUN");

    let cleanup: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(output_dir.path().join("cleanup.json"))
            .expect("unauthorized GKE must leave cleanup evidence"),
    )
    .expect("cleanup evidence must be valid JSON");
    assert_eq!(cleanup["status"], "NOT_RUN");
}

#[test]
fn one_character_nats_digest_drift_is_rejected_before_backend_start() {
    let output_dir = tempfile::tempdir().expect("create an empty benchmark output directory");
    let nats_image = pinned_nats_image();
    let drifted_nats_image = changed_digest(&nats_image);
    let output = Command::new("bash")
        .arg(runner())
        .current_dir(repo_root())
        .args(["run", "--backend", "kind"])
        .args([
            "--tape-image",
            TAPE_IMAGE,
            "--nats-image",
            drifted_nats_image.as_str(),
        ])
        .args(["--profile", "durable-v1", "--output"])
        .arg(output_dir.path())
        .output()
        .expect("start the Tape-owned benchmark runner");

    assert!(
        !output.status.success(),
        "NATS digest drift must fail closed"
    );
    let refusal = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        refusal.contains("nats image does not match durable-v1"),
        "digest drift must be rejected by profile validation: {refusal}"
    );
    assert!(
        fs::read_dir(output_dir.path())
            .expect("read output directory")
            .next()
            .is_none(),
        "digest validation must run before backend resources or reports"
    );
}

#[test]
fn durable_v1_tester_numeric_arguments_render_as_strings() {
    let template = include_str!("../scripts/jetstream-benchmark/kind/tester.yaml");
    let rendered = template
        .replace("TESTER_IMAGE", "tape-jetstream-tester:contract")
        .replace("TARGET", "tape")
        .replace("PHASE", "normal")
        .replace("PAYLOAD_BYTES", "1024")
        .replace("CLIENTS", "16")
        .replace("SAMPLE_INDEX", "3")
        .replace("RUN_ID", "contract-run")
        .replace("TOPIC", "durable-v1-1024-16-3");

    let manifest: serde_yaml::Value =
        serde_yaml::from_str(&rendered).expect("rendered tester Job must parse as YAML");
    assert_eq!(manifest["kind"], "Job");
    assert_eq!(manifest["metadata"]["name"], "durable-v1-tester");

    let args = manifest["spec"]["template"]["spec"]["containers"][0]["args"]
        .as_sequence()
        .expect("tester container args must be a YAML sequence");
    for (flag, expected) in [
        ("--payload-bytes", "1024"),
        ("--clients", "16"),
        ("--sample-index", "3"),
    ] {
        let flag_index = args
            .iter()
            .position(|arg| arg.as_str() == Some(flag))
            .unwrap_or_else(|| panic!("tester args must contain {flag}"));
        let value = args
            .get(flag_index + 1)
            .unwrap_or_else(|| panic!("tester flag {flag} must have a value"));
        assert_eq!(
            value.as_str(),
            Some(expected),
            "rendered tester value for {flag} must remain a YAML string"
        );
    }
}

#[test]
fn kind_base_kustomization_excludes_the_tester_job() {
    let source = include_str!("../scripts/jetstream-benchmark/kind/kustomization.yaml");
    let kustomization: serde_yaml::Value =
        serde_yaml::from_str(source).expect("Kind benchmark base must parse as YAML");
    assert_eq!(kustomization["kind"], "Kustomization");

    let resources = kustomization["resources"]
        .as_sequence()
        .expect("Kind benchmark base must declare resource paths");
    let includes_tester = resources
        .iter()
        .filter_map(serde_yaml::Value::as_str)
        .any(|name| name == "tester.yaml" || name.ends_with("/tester.yaml"));
    assert!(
        !includes_tester,
        "base Kind Kustomization must not deploy tester.yaml; the runner applies it per cell"
    );
}

#[test]
fn record_parser_accepts_valid_phase_order_and_identity() {
    let records = valid_benchmark_records();
    let output = run_record_parser(&records);
    assert!(
        output.status.success(),
        "valid benchmark fixture must pass parser: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parser output must be valid JSON");
    assert_eq!(report["verdict"], "PASSED");
    assert_eq!(report["records"].as_array().map(Vec::len), Some(126));
}

#[test]
fn record_parser_rejects_all_tape_first_normal_records() {
    let records = valid_benchmark_records();
    let (normal, post_restart) = records.split_at(90);
    let mut mutated: Vec<_> = normal
        .iter()
        .filter(|record| record["target"] == "tape")
        .cloned()
        .collect();
    mutated.extend(
        normal
            .iter()
            .filter(|record| record["target"] == "jetstream")
            .cloned(),
    );
    mutated.extend_from_slice(post_restart);

    assert_parser_rejects(&mutated, "normal records are not interleaved Tape/JetStream pairs");
}

#[test]
fn record_parser_rejects_missing_finalize_record() {
    let mut records = valid_benchmark_records();
    let index = records
        .iter()
        .position(|record| record["phase"] == "finalize")
        .expect("valid fixture must contain finalize records");
    records.remove(index);

    assert_parser_rejects(&records, "phase order must be normal, finalize, recovery");
}

#[test]
fn record_parser_rejects_recovery_with_sample_zero_identity() {
    let mut records = valid_benchmark_records();
    let recovery_index = records
        .iter()
        .position(|record| record["phase"] == "recovery")
        .expect("valid fixture must contain recovery records");
    let recovery = records[recovery_index].clone();
    let target = recovery["target"].as_str().expect("target string");
    let payload_bytes = recovery["cell"]["payload_bytes"]
        .as_u64()
        .expect("payload bytes integer");
    let clients = recovery["cell"]["clients"].as_u64().expect("clients integer");
    let sample_zero_index = records
        .iter()
        .position(|record| {
            record["phase"] == "normal"
                && record["target"].as_str() == Some(target)
                && record["cell"]["payload_bytes"].as_u64() == Some(payload_bytes)
                && record["cell"]["clients"].as_u64() == Some(clients)
                && record["sample_index"] == 0
        })
        .expect("valid fixture must contain sample-zero identity");
    let sample_zero = records[sample_zero_index].clone();
    let recovery_object = records[recovery_index]
        .as_object_mut()
        .expect("recovery record must be an object");
    recovery_object.insert("run_id".into(), sample_zero["run_id"].clone());
    recovery_object.insert("topic".into(), sample_zero["topic"].clone());

    assert_parser_rejects(
        &records,
        "finalize/recovery identity does not match sample-4 durable instance",
    );
}

#[test]
fn record_parser_rejects_missing_run_id_or_topic() {
    for field in ["run_id", "topic"] {
        let mut records = valid_benchmark_records();
        let record = records
            .first_mut()
            .expect("valid fixture must contain records")
            .as_object_mut()
            .expect("benchmark record must be an object");
        record.remove(field);

        assert_parser_rejects(&records, "missing durable run identity");
    }
}

#[test]
fn record_parser_rejects_nonzero_normal_replay_success() {
    let mut records = valid_benchmark_records();
    let index = records
        .iter()
        .position(|record| record["phase"] == "normal")
        .expect("valid fixture must contain normal records");
    records[index]["replay"] = serde_json::json!({
        "expected": 100_000,
        "received": 100_000,
        "loss_count": 0,
        "duplicate_count": 0,
        "error_count": 0,
        "passed": true,
    });

    assert_parser_rejects(&records, "normal records must not report replay success");
}

#[test]
fn checked_in_profile_and_assets_declare_the_fixed_durable_contract() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("scripts/jetstream-benchmark/run"))
        .expect("read benchmark runner");
    for required_term in [
        "tape-vs-jetstream-report.v1",
        "mutable image",
        "memory storage",
        "resource/workload mismatch",
        "recovery",
        "cleanup",
        "sha256",
        "128",
        "1024",
        "4096",
        "100000",
        "60",
        "five",
        "kind delete cluster",
        "delete pod",
    ] {
        assert!(
            source.contains(required_term),
            "benchmark runner must declare {required_term:?}"
        );
    }

    let profile = durable_profile();
    assert_eq!(profile["profile"], "durable-v1");
    assert_eq!(profile["architecture"], "amd64");
    assert_eq!(
        profile["payload_bytes"],
        serde_json::json!([128, 1024, 4096])
    );
    assert_eq!(profile["clients"], serde_json::json!([1, 16, 64]));
    assert_eq!(profile["warmup"], true);
    assert_eq!(profile["publish_samples"], 5);
    assert_eq!(profile["sample_seconds"], 60);
    assert_eq!(profile["persisted_backlog_replay"], 100_000);
    assert_eq!(profile["subscription_batch"], 100);
    assert_eq!(profile["ack_scoring"], "correctness_only");

    let images = profile["images"]
        .as_object()
        .expect("durable-v1 images must be an object");
    let nats_repository = images["nats_repository"]
        .as_str()
        .expect("durable-v1 must declare NATS repository");
    let nats_version = images["nats_version"]
        .as_str()
        .expect("durable-v1 must declare NATS version");
    let nats_architecture = images["nats_architecture"]
        .as_str()
        .expect("durable-v1 must declare NATS architecture");
    let nats_image = images["nats_image"]
        .as_str()
        .expect("durable-v1 must declare images.nats_image");
    assert_eq!(nats_repository, "docker.io/library/nats");
    assert_eq!(nats_version, "2.14.6");
    assert_eq!(nats_architecture, "linux/amd64");
    assert!(
        is_full_sha256_image(nats_image),
        "NATS image must be a full lowercase sha256 digest"
    );
    assert_eq!(
        nats_image.split_once('@').map(|(repository, _)| repository),
        Some(nats_repository)
    );

    for (name, required_terms) in [
        (
            "tape.yaml",
            &[
                "kind: StatefulSet",
                "replicas: 1",
                "serve, --data-dir",
                "50Gi",
                "data",
            ][..],
        ),
        (
            "nats.yaml",
            &[
                "kind: StatefulSet",
                "replicas: 1",
                "-js, -sd, /data",
                "50Gi",
                "data",
            ][..],
        ),
        (
            "tester.yaml",
            &["kind: Job", "backoffLimit: 0", "tape-bench", "durable-v1"][..],
        ),
    ] {
        let asset = fs::read_to_string(root.join("scripts/jetstream-benchmark/kind").join(name))
            .unwrap_or_else(|error| panic!("read {name}: {error}"));
        for term in required_terms {
            assert!(asset.contains(term), "{name} must declare {term:?}");
        }
    }
}
