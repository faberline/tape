use super::*;

/// R7 — readiness target + status phases (Pending / Reconciling / Ready).
#[test]
fn status_patch_reports_pending_reconciling_ready() {
    let tape = Tape::new("tape", spec(3));

    let targets = tape.readiness_targets();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].kind(), "StatefulSet");
    assert_eq!(targets[0].name(), "tape");

    let mut all_ready = HashMap::new();
    all_ready.insert("tape".to_string(), 3i64);
    let status = tape.status_patch(&ReadyFacts::new(all_ready));
    assert_eq!(status["status"]["phase"], "Ready");
    assert_eq!(status["status"]["desiredReplicas"], 3);
    assert_eq!(status["status"]["readyReplicas"], 3);

    let mut partial = HashMap::new();
    partial.insert("tape".to_string(), 1i64);
    let status = tape.status_patch(&ReadyFacts::new(partial));
    assert_eq!(status["status"]["phase"], "Reconciling");

    let status = tape.status_patch(&ReadyFacts::new(HashMap::new()));
    assert_eq!(status["status"]["phase"], "Pending");
}

/// The binary installs this process-level provider before CLI dispatch. The
/// shared helper is intentionally safe when an earlier TLS path installed it.
#[test]
fn rustls_provider_install_is_idempotent() {
    peer_tls::install_default_crypto_provider();
    peer_tls::install_default_crypto_provider();
}

/// R2/AC3 — all five conditions are present, each with a non-empty `reason`
/// and `message`.
#[test]
fn conditions_reports_all_five_with_reason_and_message() {
    let tape = Tape::new("tape", spec(3));
    let facts = tape.conditions(&ready_facts("tape", 3), &Value::Null);
    assert_eq!(facts.len(), 5, "got: {facts:?}");
    for type_ in [
        "Ready",
        "Progressing",
        "StorageHealthy",
        "BackupConfigured",
        "BackupSucceeded",
    ] {
        let c = condition(&facts, type_);
        assert!(!c.reason.is_empty(), "{type_} must carry a reason");
        assert!(!c.message.is_empty(), "{type_} must carry a message");
    }
}

/// R3/AC5 — `conditions()` is a pure function of `(spec, ready facts,
/// context)`: identical inputs called twice produce identical output.
///
/// This is what makes `observed_conditions()` safe to diff against: the
/// projection helper only preserves an existing `lastTransitionTime` when the
/// newly computed fact equals the observed one, so a `conditions()` that
/// varied per call — a clock read, a hash iteration order, a counter — would
/// restamp every condition on every 30s requeue and make the transition time
/// meaningless. `ConditionFact` carries no timestamp field of its own; the
/// stamping is entirely [`service_k8s::service::project`]'s job.
#[test]
fn conditions_is_a_pure_function_of_spec_and_observed_facts() {
    let tape = Tape::new("tape", spec(3));
    let facts_a = tape.conditions(&ready_facts("tape", 2), &Value::Null);
    let facts_b = tape.conditions(&ready_facts("tape", 2), &Value::Null);
    assert_eq!(facts_a, facts_b);
}

/// R2 — a degraded-storage instance is out of the operator's observation
/// reach today, so `StorageHealthy` reports `Unknown`/`NotObserved` rather
/// than a `False` derived from a signal that does not mean "the disk is
/// full". #3054's AC4 wanted the `False`; the observation path it needs is
/// #3071. The reasoning is on the condition in `crates/tape-operator/src/reconcile.rs`.
#[test]
fn storage_healthy_reports_unknown_not_observed() {
    let tape = Tape::new("tape", spec(3));
    let facts = tape.conditions(&ready_facts("tape", 3), &Value::Null);
    let storage = condition(&facts, "StorageHealthy");
    assert_eq!(storage.status, ConditionStatus::Unknown);
    assert_eq!(storage.reason, "NotObserved");
}

/// R2 — `BackupConfigured` names the schedule and destination when set, and
/// reports `False`/`NotConfigured` when `spec.backup` is unset.
#[test]
fn backup_configured_reflects_spec_backup() {
    let tape = Tape::new("tape", spec(3));
    let facts = tape.conditions(&ready_facts("tape", 3), &Value::Null);
    let backup = condition(&facts, "BackupConfigured");
    assert_eq!(backup.status, ConditionStatus::False);
    assert_eq!(backup.reason, "NotConfigured");

    let mut configured = spec(3);
    configured.backup = Some(configured_backup());
    let tape = Tape::new("tape", configured);
    let facts = tape.conditions(&ready_facts("tape", 3), &Value::Null);
    let backup = condition(&facts, "BackupConfigured");
    assert_eq!(backup.status, ConditionStatus::True);
    assert_eq!(backup.reason, "ScheduleConfigured");
    assert!(backup.message.contains("17 3 * * *"));
    assert!(backup.message.contains("gs://tape-backups/orders"));
}

/// AC5 anti-drift assertion: the `Ready` condition's status agrees with
/// `status_patch`'s `phase` for the same inputs, because both project from
/// the same `Observation` — this is what makes the shared struct load-bearing
/// rather than two independently-written computations that happen to agree
/// today.
#[test]
fn ready_condition_agrees_with_status_patch_phase() {
    let tape = Tape::new("tape", spec(3));
    for count in [0i64, 1, 3] {
        let facts = tape.conditions(&ready_facts("tape", count), &Value::Null);
        let ready = condition(&facts, "Ready");
        let patch = tape.status_patch(&ready_facts("tape", count));
        let phase = patch["status"]["phase"].as_str().unwrap();
        let expected_ready_status = phase == "Ready";
        assert_eq!(
            ready.status == ConditionStatus::True,
            expected_ready_status,
            "Ready condition ({:?}) disagrees with status_patch phase ({phase}) at count={count}",
            ready.status
        );
    }
}

// ---- #3071: BackupSucceeded & context tests --------------------------------

/// An RFC3339 string as the CronJob status `Time` type. Generic so this crate
/// needs no direct `k8s-openapi` dependency: the field it lands in names it.
fn parse_k8s_time<T: serde::de::DeserializeOwned>(s: &str) -> T {
    serde_json::from_value(serde_json::json!(s)).expect("valid RFC3339 time string")
}

/// E5 row 1: `spec.backup` unset yields `Unknown`/`NotConfigured`.
#[test]
fn backup_succeeded_state_table_not_configured() {
    let fact = backup_run_fact(false, None);
    assert_eq!(fact.type_, "BackupSucceeded");
    assert_eq!(fact.status, ConditionStatus::Unknown);
    assert_eq!(fact.reason, "NotConfigured");
}

/// E5 row 2: CronJob status missing yields `Unknown`/`NotObserved`.
#[test]
fn backup_succeeded_state_table_not_observed() {
    let fact = backup_run_fact(true, None);
    assert_eq!(fact.type_, "BackupSucceeded");
    assert_eq!(fact.status, ConditionStatus::Unknown);
    assert_eq!(fact.reason, "NotObserved");
}

/// E5 row 3: `status.active` non-empty yields `Unknown`/`RunInProgress`, even
/// when `lastScheduleTime` is newer than `lastSuccessfulTime`.
#[test]
fn backup_succeeded_state_table_run_in_progress() {
    let facts = CronJobStatusFacts {
        active: true,
        last_schedule: Some(parse_k8s_time("2026-07-31T10:00:00Z")),
        last_successful: Some(parse_k8s_time("2026-07-31T09:00:00Z")),
    };
    let fact = backup_run_fact(true, Some(&facts));
    assert_eq!(fact.type_, "BackupSucceeded");
    assert_eq!(fact.status, ConditionStatus::Unknown);
    assert_eq!(fact.reason, "RunInProgress");
}

/// Negative control for the row-3-over-row-5 ordering.
#[test]
fn backup_succeeded_state_table_run_in_progress_precedence_over_last_run_succeeded() {
    let facts_later = CronJobStatusFacts {
        active: true,
        last_schedule: Some(parse_k8s_time("2026-07-31T10:00:00Z")),
        last_successful: Some(parse_k8s_time("2026-07-31T10:05:00Z")),
    };
    let fact_later = backup_run_fact(true, Some(&facts_later));
    assert_eq!(fact_later.type_, "BackupSucceeded");
    assert_eq!(fact_later.status, ConditionStatus::Unknown);
    assert_eq!(fact_later.reason, "RunInProgress");

    let facts_equal = CronJobStatusFacts {
        active: true,
        last_schedule: Some(parse_k8s_time("2026-07-31T10:00:00Z")),
        last_successful: Some(parse_k8s_time("2026-07-31T10:00:00Z")),
    };
    let fact_equal = backup_run_fact(true, Some(&facts_equal));
    assert_eq!(fact_equal.type_, "BackupSucceeded");
    assert_eq!(fact_equal.status, ConditionStatus::Unknown);
    assert_eq!(fact_equal.reason, "RunInProgress");
}

/// E5 row 4: `status.lastScheduleTime` absent yields `Unknown`/`NotYetRun`.
#[test]
fn backup_succeeded_state_table_not_yet_run() {
    let facts = CronJobStatusFacts {
        active: false,
        last_schedule: None,
        last_successful: None,
    };
    let fact = backup_run_fact(true, Some(&facts));
    assert_eq!(fact.type_, "BackupSucceeded");
    assert_eq!(fact.status, ConditionStatus::Unknown);
    assert_eq!(fact.reason, "NotYetRun");
}

/// E5 row 5: `lastSuccessfulTime` present and not before `lastScheduleTime` yields `True`/`LastRunSucceeded`.
#[test]
fn backup_succeeded_state_table_last_run_succeeded() {
    let facts = CronJobStatusFacts {
        active: false,
        last_schedule: Some(parse_k8s_time("2026-07-31T10:00:00Z")),
        last_successful: Some(parse_k8s_time("2026-07-31T10:05:00Z")),
    };
    let fact = backup_run_fact(true, Some(&facts));
    assert_eq!(fact.type_, "BackupSucceeded");
    assert_eq!(fact.status, ConditionStatus::True);
    assert_eq!(fact.reason, "LastRunSucceeded");

    // Exact equal timestamp also counts as "not before" -> True
    let facts_equal = CronJobStatusFacts {
        active: false,
        last_schedule: Some(parse_k8s_time("2026-07-31T10:00:00Z")),
        last_successful: Some(parse_k8s_time("2026-07-31T10:00:00Z")),
    };
    let fact_equal = backup_run_fact(true, Some(&facts_equal));
    assert_eq!(fact_equal.status, ConditionStatus::True);
    assert_eq!(fact_equal.reason, "LastRunSucceeded");
}

/// E5 row 6: scheduled, not active, no matching success yields `False`/`LastRunFailed`.
#[test]
fn backup_succeeded_state_table_last_run_failed() {
    let facts_older = CronJobStatusFacts {
        active: false,
        last_schedule: Some(parse_k8s_time("2026-07-31T10:00:00Z")),
        last_successful: Some(parse_k8s_time("2026-07-31T09:00:00Z")),
    };
    let fact_older = backup_run_fact(true, Some(&facts_older));
    assert_eq!(fact_older.type_, "BackupSucceeded");
    assert_eq!(fact_older.status, ConditionStatus::False);
    assert_eq!(fact_older.reason, "LastRunFailed");

    let facts_no_succ = CronJobStatusFacts {
        active: false,
        last_schedule: Some(parse_k8s_time("2026-07-31T10:00:00Z")),
        last_successful: None,
    };
    let fact_no_succ = backup_run_fact(true, Some(&facts_no_succ));
    assert_eq!(fact_no_succ.status, ConditionStatus::False);
    assert_eq!(fact_no_succ.reason, "LastRunFailed");
}

/// E4: Null context yields `Unknown` status for `BackupSucceeded`, never `True` and never `False`.
#[test]
fn backup_succeeded_null_context_yields_unknown() {
    let plain = Tape::new("tape", spec(3));
    let plain_facts = plain.conditions(&ready_facts("tape", 3), &Value::Null);
    let plain_succ = condition(&plain_facts, "BackupSucceeded");
    assert_eq!(plain_succ.status, ConditionStatus::Unknown);
    assert_eq!(plain_succ.reason, "NotConfigured");

    let mut configured = spec(3);
    configured.backup = Some(configured_backup());
    let cfg = Tape::new("tape", configured);
    let cfg_facts = cfg.conditions(&ready_facts("tape", 3), &Value::Null);
    let cfg_succ = condition(&cfg_facts, "BackupSucceeded");
    assert_eq!(cfg_succ.status, ConditionStatus::Unknown);
    assert_eq!(cfg_succ.reason, "NotObserved");
}

/// E6: Purity survives with non-null context — calling `conditions` twice with identical non-null context yields identical output.
#[test]
fn conditions_is_a_pure_function_with_non_null_context() {
    let mut configured = spec(3);
    configured.backup = Some(configured_backup());
    let tape = Tape::new("tape", configured);

    let ctx = serde_json::to_value(TapeContext {
        backup_cron_job: Some(CronJobStatusFacts {
            active: false,
            last_schedule: Some(parse_k8s_time("2026-07-31T10:00:00Z")),
            last_successful: Some(parse_k8s_time("2026-07-31T10:05:00Z")),
        }),
    })
    .unwrap();

    let facts_a = tape.conditions(&ready_facts("tape", 3), &ctx);
    let facts_b = tape.conditions(&ready_facts("tape", 3), &ctx);
    assert_eq!(facts_a, facts_b);
}

/// E7: `Ready` condition and `status_patch` are independent of the context.
#[test]
fn ready_condition_and_status_patch_are_independent_of_context() {
    let mut configured = spec(3);
    configured.backup = Some(configured_backup());
    let tape = Tape::new("tape", configured);

    let ready_3 = ready_facts("tape", 3);
    let ctx_null = Value::Null;
    let ctx_succeeded = serde_json::to_value(TapeContext {
        backup_cron_job: Some(CronJobStatusFacts {
            active: false,
            last_schedule: Some(parse_k8s_time("2026-07-31T10:00:00Z")),
            last_successful: Some(parse_k8s_time("2026-07-31T10:05:00Z")),
        }),
    })
    .unwrap();
    let ctx_failed = serde_json::to_value(TapeContext {
        backup_cron_job: Some(CronJobStatusFacts {
            active: false,
            last_schedule: Some(parse_k8s_time("2026-07-31T10:00:00Z")),
            last_successful: Some(parse_k8s_time("2026-07-31T09:00:00Z")),
        }),
    })
    .unwrap();

    let patch_null = tape.status_patch(&ready_3);
    let patch_succeeded = tape.status_patch_with_context(&ready_3, &ctx_succeeded);
    let patch_failed = tape.status_patch_with_context(&ready_3, &ctx_failed);
    assert_eq!(patch_null, patch_succeeded);
    assert_eq!(patch_null, patch_failed);

    let conds_null = tape.conditions(&ready_3, &ctx_null);
    let conds_succ = tape.conditions(&ready_3, &ctx_succeeded);
    let conds_fail = tape.conditions(&ready_3, &ctx_failed);

    let ready_cond_null = condition(&conds_null, "Ready");
    let ready_cond_succ = condition(&conds_succ, "Ready");
    let ready_cond_fail = condition(&conds_fail, "Ready");
    assert_eq!(ready_cond_null, ready_cond_succ);
    assert_eq!(ready_cond_null, ready_cond_fail);
}
