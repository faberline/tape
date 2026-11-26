//! Operator-adoption render-shape tests (#1328). Compiled only with
//! `--features operator`.
//!
//! - R4: `TapeSpec` flattens `service_k8s::ClusterSpec` into the CRD schema.
//! - R5: `render` emits the downward-API StatefulSet with the exact env/probe
//!   contract tape's serve reads, plus ServiceAccount/Services/PDB.
//! - R6: the token-registry Secret wiring is opt-in (relay/lumen's pattern).
//! - R7: `status_patch` reports Pending/Reconciling/Ready.

use std::collections::HashMap;

use serde_json::Value;
use service_k8s::{ClusterSpec, ConditionStatus, ManagedService, ReadyFacts};
use tape_operator::reconcile::{backup_run_fact, CronJobStatusFacts, TapeContext};
use tape_operator::render::{prunes, render};
use tape_operator::{
    crd_yaml, AuthMode, LocalObjectReference, LogFormat, Tape, TapeBackupSpec, TapePeerTls,
    TapePlacement, TapeSpec, Toleration,
};

fn spec(replicas: u32) -> TapeSpec {
    TapeSpec {
        cluster: ClusterSpec {
            image: "tape:test".into(),
            image_pull_policy: None,
            shard_count: 1,
            replicas_per_shard: replicas,
            voter_count: replicas,
            resources: Default::default(),
        },
        storage: "10Gi".into(),
        storage_class: None,
        grace_secs: 10,
        log_level: None,
        log_format: LogFormat::default(),
        // Not a literal: the fixture must track the real default, so a future
        // flip of `AuthMode::default()` shows up in every render test at once
        // instead of leaving them asserting a shape no user ever gets (#2765).
        auth: AuthMode::default(),
        tokens_secret: None,
        tokens_secret_provider_class: None,
        tokens_secret_csi_driver: None,
        bootstrap_seed_uri: None,
        body_limit_bytes: None,
        topics: None,
        observability: false,
        backup: None,
        peer_tls: None,
        service_account_name: None,
        image_pull_secrets: Vec::new(),
        placement: None,
    }
}

fn of_kind<'a>(objs: &'a [Value], kind: &str) -> &'a Value {
    objs.iter()
        .find(|o| o["kind"] == kind)
        .unwrap_or_else(|| panic!("render output has no {kind}"))
}

fn env_of(sts: &Value) -> Vec<(&str, &Value)> {
    sts["spec"]["template"]["spec"]["containers"][0]["env"]
        .as_array()
        .expect("container env array")
        .iter()
        .map(|e| (e["name"].as_str().unwrap(), e))
        .collect()
}

// ---- #3054: prunes() ------------------------------------------------------

fn configured_backup() -> TapeBackupSpec {
    TapeBackupSpec {
        policy: service_backup::ScheduledBackupPolicy {
            schedule: "17 3 * * *".into(),
            destination: "gs://tape-backups/orders".into(),
            retention_secs: None,
        },
        admin_token_secret: None,
    }
}

/// `api_version` is part of the identity, not decoration: the controller GETs
/// each target by `(api_version, kind, name)`, so a wrong group/version makes
/// the delete address an object that does not exist and the prune silently
/// becomes a no-op. Assert all three.
fn prune_names(
    targets: &[service_k8s::service::PruneTarget],
) -> Vec<(&'static str, &'static str, String)> {
    targets
        .iter()
        .map(|t| (t.api_version(), t.kind(), t.name().to_string()))
        .collect()
}

/// The four `(observability, backup)` permutations, so every test below states
/// its expectation against the whole spec space instead of one sample.
fn permutations() -> Vec<(bool, bool, Tape)> {
    let mut out = Vec::new();
    for observability in [false, true] {
        for backup in [false, true] {
            let mut s = spec(3);
            s.observability = observability;
            s.backup = backup.then(configured_backup);
            out.push((observability, backup, Tape::new("tape", s)));
        }
    }
    out
}

// ---- #3054: conditions() ---------------------------------------------------

fn condition<'a>(
    facts: &'a [service_k8s::ConditionFact],
    type_: &str,
) -> &'a service_k8s::ConditionFact {
    facts
        .iter()
        .find(|c| c.type_ == type_)
        .unwrap_or_else(|| panic!("expected a `{type_}` condition, got: {facts:?}"))
}

fn ready_facts(name: &str, count: i64) -> ReadyFacts {
    let mut ready = HashMap::new();
    ready.insert(name.to_string(), count);
    ReadyFacts::new(ready)
}

mod conditions;
mod identity;
mod observability;
mod placement;
mod prunes;
mod render;
