//! Suite `operator_recovery`: the Tape CR `tape` of k8s/overlays/gke-test,
//! reconciled by the shipped operator into StatefulSet/tape and its one pod
//! tape-0. It replaces scripts/kind-e2e.sh: the candidate image runs in every
//! pod, an appended event survives the replacement of the serving pod with
//! its PVC kept, and the CR's status conditions follow spec.backup.

use std::time::Duration;

use serde_json::{json, Value};

use super::{
    assert_pods_run, container, endpoint, evidence, kubectl, kubectl_json, try_kubectl, wait_for,
    Expected,
};

const SUITE: &str = "operator_recovery";
const TAPE: &str = "tape";
const POD: &str = "tape-0";
const PVC: &str = "data-tape-0";
const BACKUP_CRON_JOB: &str = "tape-backup";
const SERVER_LABEL: &str =
    "app.kubernetes.io/name=tape,app.kubernetes.io/instance=tape,app.kubernetes.io/component=server";
const OPERATOR_LABEL: &str = "app.kubernetes.io/name=tape-operator";
const TOPIC: &str = "orders.recovery";
const CONDITIONS: [&str; 4] = ["Ready", "Progressing", "StorageHealthy", "BackupConfigured"];

/// The run waits only for the operator rollout. The operator creates the
/// StatefulSet after that, and pool-tape-server may still have to add a node
/// for tape-0.
const FIRST_POD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const REPLACEMENT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const RECONCILE_TIMEOUT: Duration = Duration::from_secs(2 * 60);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "e2e: needs E2E_ENDPOINT"]
async fn replay_survives_the_serving_pod_replacement() {
    let base = endpoint();
    let expected = Expected::from_env();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();

    let first_uid = wait_for_ready_pod(None, FIRST_POD_TIMEOUT);
    let kept_pvc = pvc_uid();
    evidence(
        SUITE,
        "pod ready",
        json!({ "pod": POD, "uid": first_uid, "pvc_uid": kept_pvc }),
    );
    assert_identity(&expected, "identity before replacement");

    wait_healthy(&http, &base).await;
    append(&http, &base, "before-restart").await;
    assert_replay(&http, &base, &["before-restart"]).await;
    evidence(
        SUITE,
        "append and replay",
        json!({ "topic": TOPIC, "ids": ["before-restart"] }),
    );

    kubectl(&["delete", "pod", POD, "--wait=false"]);
    let replacement_uid = wait_for_ready_pod(Some(&first_uid), REPLACEMENT_TIMEOUT);
    assert_ne!(
        replacement_uid, first_uid,
        "the StatefulSet did not replace {POD}"
    );
    assert_eq!(pvc_uid(), kept_pvc, "PVC {PVC} was replaced, not kept");
    evidence(
        SUITE,
        "pod replaced, PVC kept",
        json!({ "old_uid": first_uid, "new_uid": replacement_uid, "pvc_uid": kept_pvc }),
    );
    assert_identity(&expected, "identity after replacement");

    wait_healthy(&http, &base).await;
    assert_replay(&http, &base, &["before-restart"]).await;
    append(&http, &base, "after-restart").await;
    assert_replay(&http, &base, &["before-restart", "after-restart"]).await;
    evidence(
        SUITE,
        "ordered replay across replacement",
        json!({ "topic": TOPIC, "ids": ["before-restart", "after-restart"] }),
    );
}

/// The unit tests assert what the operator computes for status.conditions;
/// only a cluster proves it survives the CRD's status schema, the status
/// subresource, and the patch that writes it. Toggling spec.backup on the
/// same CR proves both halves of the prune round trip: the CronJob appears
/// and goes, and BackupConfigured follows the live spec.
#[test]
#[ignore = "e2e: needs E2E_ENDPOINT"]
fn status_conditions_follow_the_backup_spec() {
    endpoint();

    let conditions = wait_for(
        "all four status.conditions on Tape/tape",
        FIRST_POD_TIMEOUT,
        || {
            let tape = kubectl_json(&["get", "tape", TAPE]);
            let conditions = tape["status"]["conditions"].clone();
            let types: Vec<&str> = conditions
                .as_array()
                .map(|all| all.iter().filter_map(|c| c["type"].as_str()).collect())
                .unwrap_or_default();
            if CONDITIONS.iter().all(|want| types.contains(want)) {
                Ok(conditions)
            } else {
                Err(format!("types {types:?}"))
            }
        },
    );
    for want in CONDITIONS {
        let condition = find_condition(&conditions, want);
        let reason = condition["reason"].as_str().unwrap_or_default();
        let message = condition["message"].as_str().unwrap_or_default();
        assert!(
            !reason.is_empty() && !message.is_empty(),
            "condition {want} needs a reason and a message: {condition}"
        );
    }
    evidence(
        SUITE,
        "conditions carry reason and message",
        json!({ "conditions": conditions }),
    );

    wait_for_condition("BackupConfigured", "False");
    kubectl(&[
        "patch",
        "tape",
        TAPE,
        "--type=merge",
        "-p",
        // Once a year, so no backup Job starts while the suite runs.
        r#"{"spec":{"backup":{"schedule":"0 0 1 1 *","destination":"file:///tmp/tape-backup"}}}"#,
    ]);
    let cron_job = wait_for(
        "CronJob/tape-backup after enabling spec.backup",
        RECONCILE_TIMEOUT,
        || {
            try_kubectl(&["get", "cronjob", BACKUP_CRON_JOB, "-o", "json"])
                .map(|out| serde_json::from_str::<Value>(&out).unwrap())
        },
    );
    // spec.placement and spec.imagePullSecrets reach the backup pods too.
    let pod_spec = &cron_job["spec"]["jobTemplate"]["spec"]["template"]["spec"];
    assert_eq!(
        pod_spec["nodeSelector"]["faberline.dev/pool"], "tape-server",
        "backup pods must run on the CR's placement: {pod_spec}"
    );
    assert!(
        pod_spec["imagePullSecrets"]
            .as_array()
            .is_some_and(|all| all.iter().any(|s| s["name"] == "ghcr-pull")),
        "backup pods must pull with the CR's imagePullSecrets: {pod_spec}"
    );
    wait_for_condition("BackupConfigured", "True");
    evidence(
        SUITE,
        "backup enabled",
        json!({ "cron_job": BACKUP_CRON_JOB, "BackupConfigured": "True" }),
    );

    kubectl(&[
        "patch",
        "tape",
        TAPE,
        "--type=json",
        "-p",
        r#"[{"op":"remove","path":"/spec/backup"}]"#,
    ]);
    wait_for_condition("BackupConfigured", "False");
    wait_for(
        "CronJob/tape-backup pruned after removing spec.backup",
        RECONCILE_TIMEOUT,
        || match try_kubectl(&[
            "get",
            "cronjob",
            BACKUP_CRON_JOB,
            "--ignore-not-found",
            "-o",
            "name",
        ]) {
            Ok(out) if out.trim().is_empty() => Ok(()),
            Ok(out) => Err(out.trim().to_owned()),
            Err(err) => Err(err),
        },
    );
    evidence(
        SUITE,
        "backup removed",
        json!({ "cron_job": "pruned", "BackupConfigured": "False" }),
    );
}

/// Waits until tape-0 is Ready and is not the pod `previous` names, and
/// returns its uid.
fn wait_for_ready_pod(previous: Option<&str>, timeout: Duration) -> String {
    wait_for(&format!("pod {POD} Ready"), timeout, || {
        let pod: Value = try_kubectl(&["get", "pod", POD, "-o", "json"])
            .map(|out| serde_json::from_str(&out).unwrap())?;
        let uid = pod["metadata"]["uid"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let ready = pod["status"]["conditions"].as_array().is_some_and(|all| {
            all.iter()
                .any(|c| c["type"] == "Ready" && c["status"] == "True")
        });
        let terminating = !pod["metadata"]["deletionTimestamp"].is_null();
        if previous == Some(uid.as_str()) || terminating || !ready {
            Err(format!(
                "uid {uid}, ready {ready}, terminating {terminating}"
            ))
        } else {
            Ok(uid)
        }
    })
}

fn pvc_uid() -> String {
    let pvc = kubectl_json(&["get", "pvc", PVC]);
    pvc["metadata"]["uid"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// The candidate image runs everywhere it should: the operator pods, the
/// CR, the StatefulSet it renders, and tape-0, each pod with a runtime
/// `imageID` that binds the expected digest, and tape-0 reports the expected
/// version.
fn assert_identity(expected: &Expected, step: &str) {
    let operator = kubectl_json(&["get", "deployment", "tape-operator"]);
    let operator_replicas = operator["spec"]["replicas"].as_u64().unwrap_or(1);
    let operator_ids = assert_pods_run(expected, OPERATOR_LABEL, "operator", operator_replicas);

    let tape = kubectl_json(&["get", "tape", TAPE]);
    assert_eq!(
        tape["spec"]["image"],
        expected.image.as_str(),
        "Tape/{TAPE} spec.image"
    );
    let statefulset = kubectl_json(&["get", "statefulset", TAPE]);
    let server = container(&statefulset["spec"]["template"]["spec"], "server");
    assert_eq!(
        server["image"],
        expected.image.as_str(),
        "StatefulSet/{TAPE} server image"
    );
    let server_ids = assert_pods_run(expected, SERVER_LABEL, "server", 1);

    let version = kubectl(&["exec", POD, "-c", "server", "--", "tape", "--version"]);
    assert_eq!(
        version.trim(),
        format!("tape {}", expected.version),
        "tape --version in {POD}"
    );
    evidence(
        SUITE,
        step,
        json!({
            "image": expected.image,
            "runtime_digest": expected.runtime_digest,
            "version": expected.version,
            // tape reports no git sha at runtime yet, so this is the
            // workflow's input, tied to the image only through its tag.
            "git_sha": expected.git_sha,
            "operator_image_ids": operator_ids,
            "server_image_ids": server_ids,
        }),
    );
}

fn find_condition<'a>(conditions: &'a Value, kind: &str) -> &'a Value {
    conditions
        .as_array()
        .and_then(|all| all.iter().find(|c| c["type"] == kind))
        .unwrap_or_else(|| panic!("no condition {kind} in {conditions}"))
}

/// The condition follows the live spec, so it may take a reconcile to flip.
fn wait_for_condition(kind: &str, want: &str) {
    wait_for(
        &format!("condition {kind} {want}"),
        RECONCILE_TIMEOUT,
        || {
            let tape = kubectl_json(&["get", "tape", TAPE]);
            let conditions = &tape["status"]["conditions"];
            let got = conditions
                .as_array()
                .and_then(|all| all.iter().find(|c| c["type"] == kind))
                .and_then(|c| c["status"].as_str())
                .unwrap_or_default();
            if got == want {
                Ok(())
            } else {
                Err(format!("{kind} is {got:?}"))
            }
        },
    );
}

async fn wait_healthy(http: &reqwest::Client, base: &str) {
    let deadline = tokio::time::Instant::now() + RECONCILE_TIMEOUT;
    loop {
        let last = match http.get(format!("{base}/healthz")).send().await {
            Ok(response) if response.status().is_success() => return,
            Ok(response) => response.status().to_string(),
            Err(err) => err.to_string(),
        };
        assert!(
            tokio::time::Instant::now() < deadline,
            "{base}/healthz not healthy within {RECONCILE_TIMEOUT:?}; last saw: {last}"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn append(http: &reqwest::Client, base: &str, id: &str) {
    let response = http
        .post(format!("{base}/topics/{TOPIC}/append"))
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

async fn assert_replay(http: &reqwest::Client, base: &str, want: &[&str]) {
    let response = http
        .get(format!("{base}/topics/{TOPIC}/replay"))
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
    assert_eq!(ids, want, "replay of {TOPIC}: {body}");
}
