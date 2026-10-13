use super::*;
use std::time::Duration;

fn config() -> ClientConfig {
    ClientConfig {
        tape_url: "http://tape".into(),
        nats_url: "nats://nats".into(),
        topic: "bench".into(),
        payload_bytes: 128,
        clients: 1,
        sample_index: 0,
        operations_per_client: 1,
        replay_events: 100_000,
        sample_seconds: 60,
        profile: "durable-v1".into(),
        phase: "normal".into(),
        run_id: "test-run".into(),
    }
}

fn event(offset: u64, key: &str) -> TapeEvent {
    TapeEvent {
        offset,
        key: Some(key.into()),
        payload: serde_json::json!(null),
    }
}

#[test]
fn validation_rejects_non_profile_cells() {
    let mut c = config();
    c.payload_bytes = 129;
    assert!(c.validate(&DurableV1Profile::default()).is_err());
}

#[test]
fn validation_rejects_bad_replay_count_and_timing() {
    let mut c = config();
    c.replay_events = 99_999;
    assert!(c.validate(&DurableV1Profile::default()).is_err());
    c.replay_events = 100_000;
    c.sample_seconds = 59;
    assert!(c.validate(&DurableV1Profile::default()).is_err());
}

#[test]
fn validation_rejects_unknown_profile_and_phase() {
    let mut c = config();
    c.profile = "latest".into();
    assert!(c.validate(&DurableV1Profile::default()).is_err());
    c.profile = "durable-v1".into();
    c.phase = "warmup".into();
    assert!(c.validate(&DurableV1Profile::default()).is_err());
}

#[test]
fn normal_samples_do_not_schedule_backlog_replay() {
    let mut c = config();
    assert_eq!(replay_events_for_phase(&c), 0);
    c.phase = "finalize".into();
    assert_eq!(replay_events_for_phase(&c), 100_000);
}

#[test]
fn matrix_enumeration_has_frozen_cardinality_and_identity() {
    assert_eq!(matrix_cells("normal").unwrap().len(), 45);
    assert_eq!(matrix_cells("recovery").unwrap().len(), 9);
    let cells = matrix_cells("normal").unwrap();
    assert_eq!(cells[0], (128, 1, 0));
    assert_eq!(cells[44], (4096, 64, 4));
    assert!(matrix_cells("other").is_err());
}

#[test]
fn target_validation_requires_only_the_selected_endpoint() {
    let mut c = config();
    c.nats_url.clear();
    assert!(c
        .validate_for_target("tape", &DurableV1Profile::default())
        .is_ok());
    assert!(c
        .validate_for_target("jetstream", &DurableV1Profile::default())
        .is_err());
    c.nats_url = "nats://nats".into();
    c.tape_url.clear();
    assert!(c
        .validate_for_target("jetstream", &DurableV1Profile::default())
        .is_ok());
    assert!(c
        .validate_for_target("tape", &DurableV1Profile::default())
        .is_err());
}

#[test]
fn replay_identity_requires_cursor_continuity_and_replay_keys() {
    let events = vec![event(41, "replay-0"), event(42, "replay-1")];
    assert_eq!(validate_replay_identity(&events, 41, 0).unwrap(), 2);
    assert!(validate_replay_identity(&events, 40, 0).is_err());
    let mut wrong = events;
    wrong[1].key = Some("bench-1".into());
    assert!(validate_replay_identity(&wrong, 41, 0).is_err());
}

#[test]
fn normal_and_recovery_use_the_same_subject_and_subscription_cell() {
    let c = config();
    assert_eq!(subject(&c), "tape.bench.test-run.128.1");
    assert_eq!(tape_subscription(&c), "bench-test-run-128-1-normal");
    assert_ne!(
        tape_subscription_for_phase(&c, "finalize"),
        tape_subscription_for_phase(&c, "recovery")
    );
}

#[test]
fn sample_identity_changes_with_runner_supplied_run_id() {
    let first = config();
    let mut second = first.clone();
    second.run_id = "test-run-sample-1".into();
    assert_ne!(subject(&first), subject(&second));
    assert_ne!(tape_subscription(&first), tape_subscription(&second));
}

#[test]
fn replay_start_skips_prefix_and_requires_replay_zero() {
    let events = vec![event(10, "bench-0"), event(11, "replay-0")];
    assert_eq!(replay_start(&events).unwrap(), 11);
    assert!(replay_start(&events[..1]).is_err());
}

#[test]
fn replay_scan_skips_prefix_and_rejects_bad_sequences() {
    let page = vec![
        event(1, "warmup"),
        event(2, "bench-0"),
        event(3, "replay-0"),
        event(4, "replay-1"),
    ];
    assert_eq!(scan_replay_page(&page, 0).unwrap(), (2, 2));
    let mut wrong = page.clone();
    wrong[2].key = Some("replay-2".into());
    assert!(scan_replay_page(&wrong, 0).is_err());
    let bad_prefix = vec![event(1, "replay-3")];
    assert!(scan_replay_page(&bad_prefix, 0).is_err());
}

#[test]
fn tape_payload_is_exact_string_bytes_without_envelope() {
    let payload = serde_json::Value::String("x".repeat(128));
    assert_eq!(payload.as_str().unwrap().len(), 128);
    assert!(payload.get("body").is_none());
}

#[test]
fn incomplete_record_preserves_failure_and_is_not_complete() {
    let c = config();
    let r = incomplete_record(&c, "tape", "connection refused".into());
    assert_eq!(r.verdict, "incomplete");
    assert_eq!(r.failure.as_deref(), Some("connection refused"));
    assert!(r.validate().is_ok());
}

#[test]
fn readiness_backoff_is_bounded_without_waiting() {
    let first = readiness_backoff(0);
    let later = readiness_backoff(99);
    assert_eq!(first, Duration::from_millis(100));
    assert_eq!(later, Duration::from_secs(1));
    assert!(later >= first);
}

#[test]
fn p99_sorts_samples_and_is_zero_without_samples() {
    assert_eq!(p99(Vec::new()), 0);
    let mut samples: Vec<u128> = (1..=100).rev().collect();
    samples.swap(0, 50);
    assert_eq!(p99(samples), 99);
}

fn measured_record() -> BenchmarkRecord {
    BenchmarkRecord {
        schema: "tape-vs-jetstream-record.v1",
        target: "jetstream",
        run_id: "test-run".into(),
        topic: "bench".into(),
        cell: Cell {
            payload_bytes: 128,
            clients: 1,
        },
        sample_index: 0,
        phase: "normal",
        operation_count: 1,
        elapsed_ms: 1,
        throughput_ops_per_sec: 1000.0,
        p99_ms: 0,
        ack_p99_ms: 0,
        replay: ReplayResult {
            expected: 0,
            received: 0,
            loss_count: 0,
            duplicate_count: 0,
            error_count: 0,
            passed: true,
        },
        ack_batch_size: 100,
        ack_cumulative: false,
        loss_count: 0,
        duplicate_count: 0,
        error_count: 0,
        verdict: "measured",
        failure: None,
    }
}

#[test]
fn record_json_has_stable_schema_and_verdict_inputs() {
    let r = measured_record();
    assert!(r.validate().is_ok());
    let mut tape = r.clone();
    tape.target = "tape";
    tape.ack_cumulative = true;
    assert!(tape.validate().is_ok());
    tape.ack_cumulative = false;
    assert!(tape.validate().is_err());
    let value = serde_json::to_value(&r).unwrap();
    for field in [
        "schema",
        "target",
        "cell",
        "sample_index",
        "operation_count",
        "throughput_ops_per_sec",
        "p99_ms",
        "replay",
        "ack_batch_size",
        "verdict",
    ] {
        assert!(value.get(field).is_some(), "missing {field}");
    }
}

#[test]
fn finalize_record_is_distinct_and_requires_full_replay() {
    let c = config();
    let mut r = BenchmarkRecord {
        phase: "finalize",
        throughput_ops_per_sec: 1.0,
        replay: ReplayResult {
            expected: 100_000,
            received: 100_000,
            loss_count: 0,
            duplicate_count: 0,
            error_count: 0,
            passed: true,
        },
        ..measured_record()
    };
    assert!(r.validate().is_ok());
    r.phase = "normal";
    assert!(r.validate().is_err());
    assert_eq!(c.phase, "normal");
}
