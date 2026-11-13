//! Suite `operator_restore`: a drill that restores a Tape group from a backup
//! on a gs:// bucket. The Tape CR `tape` of k8s/overlays/gke-test runs three
//! replicas; the backup CronJob ships a snapshot to the bucket; the group and
//! its PVCs are destroyed; the CR is created again with
//! `spec.bootstrapSeedUri` set to the backup object. Every offset 0..N must
//! replay back, in order, and the restored group must still accept writes.
//! The bucket comes from `E2E_BACKUP_BUCKET`; aw creates it for the drill.

use std::time::Duration;

use serde_json::{json, Value};

use super::{
    append, assert_replay, endpoint, evidence, kubectl, kubectl_json, required_env, try_kubectl,
    wait_for, wait_healthy,
};

const SUITE: &str = "operator_restore";
const TAPE: &str = "tape";
const TOPIC: &str = "restore.drill";
const COUNT: usize = 30;
const BACKUP_JOB: &str = "tape-backup-drill";
const BACKUP_CRON_JOB: &str = "tape-backup";
const PREFIX: &str = "restore-drill";
const RESTORE_SA: &str = "tape-restore-drill";
const SERVER_LABEL: &str =
    "app.kubernetes.io/name=tape,app.kubernetes.io/instance=tape,app.kubernetes.io/component=server";
const SERVER_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const BACKUP_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const RESTORE_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const GRACE_SECS: u64 = 10;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "e2e: needs E2E_ENDPOINT"]
async fn backup_destroy_restore_brings_every_offset_back() {
    let base = endpoint();
    let bucket = required_env("E2E_BACKUP_BUCKET");
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();
    let ids: Vec<String> = (1..=COUNT).map(|i| format!("r-{i:04}")).collect();
    let want: Vec<&str> = ids.iter().map(String::as_str).collect();

    // a. Scale to three replicas and set a yearly backup schedule, so no
    // backup Job starts on its own. The destination is the drill prefix.
    let patch = json!({
        "spec": {
            "replicasPerShard": 3,
            "voterCount": 3,
            "graceSecs": GRACE_SECS,
            "backup": {
                "schedule": "0 0 1 1 *",
                "destination": format!("gs://{bucket}/{PREFIX}")
            }
        }
    });
    kubectl(&[
        "patch",
        "tape",
        TAPE,
        "--type=merge",
        "-p",
        &patch.to_string(),
    ]);
    evidence(
        SUITE,
        "patched tape to 3 replicas with backup",
        json!({ "replicasPerShard": 3, "voterCount": 3, "destination": format!("gs://{bucket}/{PREFIX}") }),
    );

    // c. Wait for the pods, the server, and the backup CronJob.
    let pods = wait_ready_pods(3, SERVER_TIMEOUT);
    wait_healthy(&http, &base, SERVER_TIMEOUT).await;
    wait_for("CronJob tape-backup", Duration::from_secs(2 * 60), || {
        try_kubectl(&["get", "cronjob", BACKUP_CRON_JOB]).map(|_| ())
    });
    evidence(SUITE, "group ready", json!({ "pods": pods }));

    // d. Publish the events and check them.
    for id in &ids {
        append(&http, &base, TOPIC, id).await;
    }
    assert_replay(&http, &base, TOPIC, &want).await;
    evidence(
        SUITE,
        "appended and replayed before backup",
        json!({ "topic": TOPIC, "count": COUNT }),
    );

    // e. Run one backup from the CronJob and read the object key it wrote.
    kubectl(&["create", "job", BACKUP_JOB, "--from=cronjob/tape-backup"]);
    wait_for(
        &format!("Job {BACKUP_JOB} succeeded"),
        BACKUP_TIMEOUT,
        || {
            let job: Value = try_kubectl(&["get", "job", BACKUP_JOB, "-o", "json"])
                .map(|out| serde_json::from_str(&out).unwrap())?;
            if job["status"]["failed"].as_u64().unwrap_or(0) >= 1 {
                let log = try_kubectl(&["logs", &format!("job/{BACKUP_JOB}")]).unwrap_or_default();
                panic!("Job {BACKUP_JOB} failed: {log}");
            }
            if job["status"]["succeeded"].as_u64() == Some(1) {
                Ok(())
            } else {
                Err(job["status"].to_string())
            }
        },
    );
    let log = kubectl(&["logs", &format!("job/{BACKUP_JOB}")]);
    let key = backup_object_key(&log).unwrap_or_else(|e| panic!("{e}: {log}"));
    let seed = seed_uri(&bucket, &key);
    evidence(SUITE, "backup done", json!({ "key": key, "seed": seed }));

    // f. Save the CR and the PVCs as they are now.
    let saved = kubectl_json(&["get", "tape", TAPE]);
    let old_pvcs = pvc_items();
    assert_eq!(old_pvcs.len(), 3, "PVCs before the drill: {old_pvcs:?}");
    let old_uids: Vec<String> = old_pvcs.iter().map(|(_, uid)| uid.clone()).collect();

    // g. Destroy the group and its PVCs.
    kubectl(&["delete", "tape", TAPE, "--wait=true"]);
    wait_for("server pods gone", TEARDOWN_TIMEOUT, || {
        let pods: Value = try_kubectl(&["get", "pods", "-l", SERVER_LABEL, "-o", "json"])
            .map(|out| serde_json::from_str(&out).unwrap())?;
        let live = pods["items"]
            .as_array()
            .map(|all| {
                all.iter()
                    .filter(|p| p["metadata"]["deletionTimestamp"].is_null())
                    .count()
            })
            .unwrap_or(0);
        if live == 0 {
            Ok(())
        } else {
            Err(format!("{live} server pods"))
        }
    });
    kubectl(&[
        "delete",
        "pvc",
        "-l",
        "app.kubernetes.io/instance=tape",
        "--wait=true",
    ]);
    wait_for("PVCs gone", TEARDOWN_TIMEOUT, || {
        let names = pvc_names();
        if names.is_empty() {
            Ok(())
        } else {
            Err(format!("{names:?}"))
        }
    });
    evidence(
        SUITE,
        "group and PVCs destroyed",
        json!({ "old_pvcs": old_pvcs.iter().map(|(name, _)| name).collect::<Vec<_>>() }),
    );

    // h. Create the CR again, seeded from the backup object.
    let mut restore = saved;
    if let Some(meta) = restore["metadata"].as_object_mut() {
        for key in [
            "resourceVersion",
            "uid",
            "creationTimestamp",
            "generation",
            "managedFields",
            "finalizers",
        ] {
            meta.remove(key);
        }
        if let Some(annotations) = meta.get_mut("annotations").and_then(Value::as_object_mut) {
            annotations.remove("kubectl.kubernetes.io/last-applied-configuration");
        }
    }
    if let Some(object) = restore.as_object_mut() {
        object.remove("status");
    }
    if let Some(spec) = restore["spec"].as_object_mut() {
        spec.remove("backup");
        spec.insert("bootstrapSeedUri".to_owned(), json!(seed));
        // The server pods read the seed with their own identity. The bucket
        // grants that identity, not the default `tape` ServiceAccount.
        spec.insert("serviceAccountName".to_owned(), json!(RESTORE_SA));
    }
    let dir = std::env::var("TMPDIR").unwrap_or_else(|_| "/work".to_owned());
    let path = format!("{dir}/tape-restore.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&restore).unwrap())
        .unwrap_or_else(|err| panic!("write {path}: {err}"));
    kubectl(&["create", "-f", &path]);

    // i. Wait for the restored group and check its new PVCs.
    let restored_pods = wait_ready_pods(3, RESTORE_TIMEOUT);
    wait_healthy(&http, &base, RESTORE_TIMEOUT).await;
    let pods: Value = kubectl_json(&["get", "pods", "-l", SERVER_LABEL]);
    for pod in pods["items"].as_array().into_iter().flatten() {
        assert_eq!(
            pod["spec"]["serviceAccountName"], RESTORE_SA,
            "restored pod {} runs as the wrong ServiceAccount",
            pod["metadata"]["name"]
        );
    }
    let new_pvcs = pvc_items();
    assert_eq!(new_pvcs.len(), 3, "PVCs after the restore: {new_pvcs:?}");
    for (name, uid) in &new_pvcs {
        assert!(
            !old_uids.contains(uid),
            "PVC {name} was not recreated (uid {uid} is an old one)"
        );
    }

    // j. Every offset 0..N is back, in order.
    assert_replay(&http, &base, TOPIC, &want).await;
    evidence(
        SUITE,
        "restored",
        json!({ "count": COUNT, "seed": seed, "pods": restored_pods, "service_account": RESTORE_SA }),
    );

    // k. The restored group still accepts writes.
    append(&http, &base, TOPIC, "r-after").await;
    let mut after = want.clone();
    after.push("r-after");
    assert_replay(&http, &base, TOPIC, &after).await;
    evidence(
        SUITE,
        "writes after restore",
        json!({ "topic": TOPIC, "ids": ["r-after"] }),
    );
}

/// The `object.key` of the backup, read from the pretty JSON that
/// `tape backup` prints (`BackupRunResult`).
fn backup_object_key(log: &str) -> Result<String, String> {
    let start = log
        .find('{')
        .ok_or_else(|| "no JSON in the backup log".to_owned())?;
    let value = serde_json::Deserializer::from_str(&log[start..])
        .into_iter::<Value>()
        .next()
        .ok_or_else(|| "no JSON in the backup log".to_owned())?
        .map_err(|err| format!("invalid backup JSON: {err}"))?;
    value["object"]["key"]
        .as_str()
        .filter(|key| !key.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "the backup JSON has no object.key".to_owned())
}

/// The `bootstrapSeedUri` of a backup object: `gs://<bucket>/<prefix>/<key>`,
/// where the key may already include the prefix.
fn seed_uri(bucket: &str, key: &str) -> String {
    if key.starts_with(&format!("{PREFIX}/")) {
        format!("gs://{bucket}/{key}")
    } else {
        format!("gs://{bucket}/{PREFIX}/{key}")
    }
}

/// Waits until exactly `want` live server pods exist and every one is Ready,
/// and returns their sorted names.
fn wait_ready_pods(want: usize, timeout: Duration) -> Vec<String> {
    wait_for(&format!("{want} Ready server pods"), timeout, || {
        let pods: Value = try_kubectl(&["get", "pods", "-l", SERVER_LABEL, "-o", "json"])
            .map(|out| serde_json::from_str(&out).unwrap())?;
        let live: Vec<&Value> = pods["items"]
            .as_array()
            .map(|all| {
                all.iter()
                    .filter(|p| p["metadata"]["deletionTimestamp"].is_null())
                    .collect()
            })
            .unwrap_or_default();
        let ready = |pod: &Value| {
            pod["status"]["conditions"].as_array().is_some_and(|all| {
                all.iter()
                    .any(|c| c["type"] == "Ready" && c["status"] == "True")
            })
        };
        let ready_count = live.iter().filter(|pod| ready(pod)).count();
        if live.len() == want && ready_count == want {
            let mut names: Vec<String> = live
                .iter()
                .filter_map(|p| p["metadata"]["name"].as_str().map(str::to_owned))
                .collect();
            names.sort();
            Ok(names)
        } else {
            Err(format!("{} live, {ready_count} Ready", live.len()))
        }
    })
}

/// The PVCs of the group as (name, uid), sorted by name.
fn pvc_items() -> Vec<(String, String)> {
    let pvcs = kubectl_json(&["get", "pvc", "-l", "app.kubernetes.io/instance=tape"]);
    let mut items: Vec<(String, String)> = pvcs["items"]
        .as_array()
        .map(|all| {
            all.iter()
                .filter_map(|p| {
                    Some((
                        p["metadata"]["name"].as_str()?.to_owned(),
                        p["metadata"]["uid"].as_str()?.to_owned(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    items.sort();
    items
}

/// The names of the group's PVCs, sorted.
fn pvc_names() -> Vec<String> {
    pvc_items().into_iter().map(|(name, _)| name).collect()
}

#[cfg(test)]
mod tests {
    use super::{backup_object_key, seed_uri};

    #[test]
    fn backup_object_key_reads_the_pretty_json() {
        let log = "some line\n{\n  \"object\": { \"key\": \"restore-drill/tape-1.snap\", \"bytes\": 12 }\n}\n";
        assert_eq!(
            backup_object_key(log),
            Ok("restore-drill/tape-1.snap".to_owned())
        );
    }

    #[test]
    fn backup_object_key_rejects_a_log_without_json() {
        assert!(backup_object_key("error: boom").is_err());
    }

    #[test]
    fn backup_object_key_rejects_an_empty_key() {
        assert!(backup_object_key("{\"object\":{\"key\":\"\"}}").is_err());
    }

    #[test]
    fn seed_uri_keeps_a_key_that_has_the_prefix() {
        assert_eq!(
            seed_uri("b", "restore-drill/x.snap"),
            "gs://b/restore-drill/x.snap"
        );
    }

    #[test]
    fn seed_uri_adds_the_prefix_to_a_bare_key() {
        assert_eq!(seed_uri("b", "x.snap"), "gs://b/restore-drill/x.snap");
    }
}
