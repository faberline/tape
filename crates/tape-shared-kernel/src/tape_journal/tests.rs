use super::*;

#[test]
fn append_and_replay_by_offset_and_time() {
    let mut journal = TapeJournal::default();
    journal.append_at(
        "orders",
        Some("a".into()),
        serde_json::json!({"n": 1}),
        100,
        100,
    );
    journal.append_at(
        "orders",
        Some("b".into()),
        serde_json::json!({"n": 2}),
        200,
        200,
    );

    let by_offset = journal.replay("orders", Some(1), None, None);
    assert_eq!(by_offset.len(), 1);
    assert_eq!(by_offset[0].payload, serde_json::json!({"n": 2}));

    let by_time = journal.replay("orders", None, Some(150), Some(1));
    assert_eq!(by_time.len(), 1);
    assert_eq!(by_time[0].offset, 1);
}

#[test]
fn checkpoints_advance_and_reject_stale_offsets() {
    let mut journal = TapeJournal::default();
    journal.append_at("orders", None, serde_json::json!({"n": 1}), 100, 100);
    journal.append_at("orders", None, serde_json::json!({"n": 2}), 200, 200);

    let checkpoint = journal
        .put_checkpoint_at("orders", "worker-a", 1, 0)
        .unwrap();
    assert_eq!(checkpoint.offset, 1);
    assert_eq!(journal.checkpoint("orders", "worker-a").unwrap().offset, 1);
    assert!(matches!(
        journal.put_checkpoint_at("orders", "worker-a", 0, 0),
        Err(TapeError::StaleCheckpoint { .. })
    ));
    assert!(matches!(
        journal.put_checkpoint_at("orders", "worker-a", 3, 0),
        Err(TapeError::CheckpointBeyondEnd { .. })
    ));
}

#[test]
fn pull_subscription_preserves_checkpoint_compatibility() {
    let mut journal = TapeJournal::default();
    journal.append_at("orders", None, serde_json::json!({"n": 1}), 100, 100);
    let checkpoint = journal
        .put_checkpoint_at("orders", "worker-a", 1, 0)
        .unwrap();

    let subscription = journal.create_subscription("orders", "worker-a").unwrap();
    assert_eq!(subscription.name, "worker-a");
    assert_eq!(journal.checkpoint("orders", "worker-a"), Some(&checkpoint));

    let deleted = journal.delete_subscription("orders", "worker-a").unwrap();
    assert_eq!(deleted.name, "worker-a");
    assert_eq!(journal.checkpoint("orders", "worker-a"), Some(&checkpoint));
}

#[test]
fn pull_subscription_uses_checkpoint_cursor_and_never_implicitly_acks() {
    let mut journal = TapeJournal::default();
    for offset in 0..3 {
        journal.append_at(
            "orders",
            None,
            serde_json::json!({"offset": offset}),
            100,
            100,
        );
    }
    journal.create_subscription("orders", "worker-a").unwrap();
    journal
        .put_checkpoint_at("orders", "worker-a", 1, 0)
        .unwrap();

    let batch = journal
        .pull_subscription("orders", "worker-a", Some(2))
        .unwrap();
    assert_eq!(batch.cursor, 1);
    assert_eq!(
        batch
            .events
            .iter()
            .map(|event| event.offset)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(batch.next_offset, 3);
    assert_eq!(journal.checkpoint("orders", "worker-a").unwrap().offset, 1);
}

#[test]
fn pull_subscription_ack_reuses_checkpoint_guards() {
    let mut journal = TapeJournal::default();
    journal.append_at("orders", None, serde_json::json!({"n": 1}), 100, 100);
    journal.create_subscription("orders", "worker-a").unwrap();
    assert!(matches!(
        journal.ack_subscription_at("orders", "worker-a", 2, 0),
        Err(SubscriptionAckError::Checkpoint(
            TapeError::CheckpointBeyondEnd { .. }
        ))
    ));
    journal
        .ack_subscription_at("orders", "worker-a", 1, 0)
        .unwrap();
    assert!(matches!(
        journal.ack_subscription_at("orders", "worker-a", 0, 0),
        Err(SubscriptionAckError::Checkpoint(
            TapeError::StaleCheckpoint { .. }
        ))
    ));
}

#[test]
fn pull_subscription_rejects_oversized_window() {
    let mut journal = TapeJournal::default();
    journal.append_at("orders", None, serde_json::json!({"n": 1}), 100, 100);
    journal.create_subscription("orders", "worker-a").unwrap();

    assert!(matches!(
        journal.pull_subscription("orders", "worker-a", Some(MAX_PULL_BATCH + 1)),
        Err(SubscriptionError::PullBatchTooLarge { .. })
    ));
    assert!(journal.checkpoint("orders", "worker-a").is_none());
}

#[test]
fn retention_prunes_history_without_rewinding_offsets_and_protects_consumers() {
    let mut journal = TapeJournal::default();
    for offset in 0..5 {
        journal.append_at(
            "orders",
            None,
            serde_json::json!({"offset": offset}),
            1_000 + offset * 1_000,
            5_000,
        );
    }
    journal
        .put_checkpoint_at("orders", "audit", 2, 5_000)
        .unwrap();
    let outcome = journal.put_retention(
        "orders",
        RetentionPolicy {
            min_offset: Some(4),
            max_age_seconds: None,
            protected_consumers: vec!["audit".into()],
        },
        5_000,
    );
    assert_eq!(outcome.earliest_offset, 2);
    assert_eq!(outcome.removed, 2);
    assert_eq!(
        journal
            .replay("orders", None, None, None)
            .into_iter()
            .map(|event| event.offset)
            .collect::<Vec<_>>(),
        vec![2, 3, 4]
    );

    let appended = journal.append_at(
        "orders",
        None,
        serde_json::json!({"offset": 5}),
        6_000,
        6_000,
    );
    assert_eq!(appended.offset, 5, "retention must not reuse offsets");
}
