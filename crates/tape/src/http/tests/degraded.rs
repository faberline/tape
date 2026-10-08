//! #2573 degraded read-only mode: the first ENOSPC latches it, later
//! mutations fast-fail, reads keep serving, and clearing it restores writes.

use super::*;

/// Build a state whose `--store` points at `path`, carrying one event on
/// `topic` so the persisted journal has content worth losing.
fn state_with_store(path: &std::path::Path, topic: &str) -> AppState {
    let mut journal = TapeJournal::default();
    journal.append(topic, None, serde_json::json!({ "n": 1 }), None);
    AppState::new(journal, Some(path.to_path_buf()), 8 * 1024 * 1024)
}

/// #2573 AC1-AC3 — the whole degraded-mode contract in one journey: the
/// first ENOSPC answers a typed 507 and latches degraded mode, every
/// later mutation is fast-failed *before* the journal is touched, and
/// reads keep serving throughout.
///
/// The ENOSPC itself comes from the `#[cfg(test)]` injection seam rather
/// than a real full disk: filling a tmpfs from a unit test is neither
/// hermetic nor portable, and what is under test is the reaction to
/// `io::ErrorKind::StorageFull`, not the kernel's ability to produce it.
/// #2572 is what makes the seam faithful — it preserves the ErrorKind
/// through `atomic_write`'s context chain, so the real path reaches this
/// same branch with the same kind.
#[tokio::test]
async fn enospc_latches_degraded_mode_fast_fails_mutations_and_keeps_reads_serving() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.json");
    let state = state_with_store(&path, "orders");
    let app = router(state.clone());

    // Baseline: a healthy node appends and is not degraded.
    let healthy = post_json(
        app.clone(),
        "/topics/orders/append",
        &serde_json::json!({ "payload": { "n": 2 } }),
    )
    .await;
    assert_eq!(healthy.0, StatusCode::OK);
    assert!(!state.metrics().is_storage_degraded());

    // AC1: the persist that hits ENOSPC answers 507 with the typed
    // `storage_full` kind — not a generic 500 a client would retry into.
    state.service().set_inject_storage_full(true);
    let first = post_json(
        app.clone(),
        "/topics/orders/append",
        &serde_json::json!({ "payload": { "n": 3 } }),
    )
    .await;
    assert_eq!(first.0, StatusCode::INSUFFICIENT_STORAGE);
    assert!(
        first.1.contains("storage_full"),
        "the error kind must be typed, got: {}",
        first.1
    );
    assert!(state.metrics().is_storage_degraded(), "the flag is sticky");
    assert_eq!(state.metrics().storage_full_errors_total.get(), 1);

    // AC2: further mutations short-circuit at the gate. The event count
    // staying put is the proof they never reached the journal — a gate
    // that ran after the mutation would leave the in-memory journal
    // drifting ahead of the durable one on every rejected request.
    let events_after_first_failure = state
        .journal_handle()
        .lock()
        .unwrap()
        .replay("orders", None, None, None)
        .len();
    for (label, response) in [
        (
            "append",
            post_json(
                app.clone(),
                "/topics/orders/append",
                &serde_json::json!({ "payload": { "n": 4 } }),
            )
            .await,
        ),
        (
            "checkpoint advance",
            put_json(
                app.clone(),
                "/topics/orders/consumers/c1/checkpoint",
                &serde_json::json!({ "offset": 1 }),
            )
            .await,
        ),
        (
            "subscription create",
            post_json(
                app.clone(),
                "/topics/orders/subscriptions",
                &serde_json::json!({ "name": "audit" }),
            )
            .await,
        ),
        (
            "retention set",
            put_json(
                app.clone(),
                "/topics/orders/retention",
                &serde_json::json!({ "max_events": 10 }),
            )
            .await,
        ),
    ] {
        assert_eq!(
            response.0,
            StatusCode::INSUFFICIENT_STORAGE,
            "{label} must be fast-failed while degraded, got: {}",
            response.1
        );
    }
    assert_eq!(
        state
            .journal_handle()
            .lock()
            .unwrap()
            .replay("orders", None, None, None)
            .len(),
        events_after_first_failure,
        "fast-failed mutations must not touch the journal"
    );
    assert_eq!(
        state.metrics().storage_full_errors_total.get(),
        1,
        "the gate never reaches the durable path, so it counts no new ENOSPC hits"
    );

    // AC3: degraded is read-ONLY, not down. Replay keeps answering — a
    // full disk is precisely when an operator most needs to read what is
    // already journalled.
    let replay = get(app, "/topics/orders/replay").await;
    assert_eq!(replay.0, StatusCode::OK);
}

/// #2573 AC4 (unit half) — clearing degraded mode returns the node to
/// normal service with no restart. This drives `clear_storage_degraded`
/// directly, which is exactly what the periodic re-probe task in
/// `tape serve` (`spawn_storage_full_reprobe`) calls once a probe
/// write into the store directory succeeds; the timer itself is not worth
/// a 30s unit test.
#[tokio::test]
async fn leaving_degraded_mode_restores_mutations_without_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("journal.json");
    let state = state_with_store(&path, "orders");
    let app = router(state.clone());

    state.service().set_inject_storage_full(true);
    let blocked = post_json(
        app.clone(),
        "/topics/orders/append",
        &serde_json::json!({ "payload": { "n": 2 } }),
    )
    .await;
    assert_eq!(blocked.0, StatusCode::INSUFFICIENT_STORAGE);

    // The disk got bigger / something got freed; the re-probe succeeds.
    state.service().set_inject_storage_full(false);
    state.metrics().clear_storage_degraded();

    let recovered = post_json(
        app,
        "/topics/orders/append",
        &serde_json::json!({ "payload": { "n": 3 } }),
    )
    .await;
    assert_eq!(recovered.0, StatusCode::OK, "same process, no restart");
    assert!(path.exists(), "and the journal is durable again");
    assert_eq!(
        state.metrics().storage_full_errors_total.get(),
        1,
        "recovery does not erase the record that this node was once full"
    );
}
