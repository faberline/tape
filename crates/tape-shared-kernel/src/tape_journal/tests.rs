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

#[test]
fn ack_with_expected_offset_is_a_compare_and_set() {
    let mut journal = TapeJournal::default();
    for n in 0..3 {
        journal.append_at("orders", None, serde_json::json!({"n": n}), 100, 100);
    }
    journal.create_subscription("orders", "worker-a").unwrap();

    // a. no checkpoint counts as 0, so expected 0 matches and the ack advances.
    let ack = journal
        .ack_subscription_expected_at("orders", "worker-a", 1, Some(0), 0)
        .unwrap();
    assert_eq!(ack.offset, 1);

    // b. expected 0 is stale (current is 1): 409-class error, cursor unchanged.
    assert_eq!(
        journal.ack_subscription_expected_at("orders", "worker-a", 3, Some(0), 0),
        Err(SubscriptionAckError::Checkpoint(
            TapeError::ExpectedOffsetMismatch {
                expected_offset: 0,
                current_offset: 1,
            }
        ))
    );
    assert_eq!(journal.checkpoint("orders", "worker-a").unwrap().offset, 1);

    // c. expected equals the current cursor: the ack advances.
    let ack = journal
        .ack_subscription_expected_at("orders", "worker-a", 3, Some(1), 0)
        .unwrap();
    assert_eq!(ack.offset, 3);

    // d. no expected offset keeps the old behaviour (same offset is idempotent).
    let ack = journal
        .ack_subscription_expected_at("orders", "worker-a", 3, None, 0)
        .unwrap();
    assert_eq!(ack.offset, 3);
}

#[test]
fn two_subscriptions_keep_independent_cursors() {
    let mut journal = TapeJournal::default();
    journal.append_at("orders", None, serde_json::json!({"n": 1}), 100, 100);
    journal.append_at("orders", None, serde_json::json!({"n": 2}), 200, 200);
    journal.create_subscription("orders", "a").unwrap();
    journal.create_subscription("orders", "b").unwrap();

    journal.ack_subscription_at("orders", "a", 2, 0).unwrap();

    let b = journal.pull_subscription("orders", "b", None).unwrap();
    assert_eq!(b.cursor, 0);
    assert_eq!(b.events.len(), 2);
    let a = journal.pull_subscription("orders", "a", None).unwrap();
    assert_eq!(a.cursor, 2);
}

fn seeded_journal() -> TapeJournal {
    let mut journal = TapeJournal::default();
    for i in 0..3 {
        journal.append_at("orders", None, serde_json::json!({"n": i}), 100, 100);
    }
    journal.create_subscription("orders", "a").unwrap();
    journal
}

#[test]
fn seek_rewinds_the_cursor_and_bumps_the_epoch() {
    let mut journal = seeded_journal();
    journal.ack_subscription_at("orders", "a", 3, 0).unwrap();

    let outcome = journal.seek_subscription_at("orders", "a", 1, 10).unwrap();
    assert_eq!(
        outcome,
        SeekOutcome {
            topic: "orders".into(),
            subscription: "a".into(),
            epoch: 1,
            cursor: 1,
            earliest_offset: 0,
            end_offset: 3,
        }
    );
    let batch = journal.pull_subscription("orders", "a", None).unwrap();
    assert_eq!(batch.cursor, 1);
    assert_eq!(batch.epoch, 1);
    let offsets: Vec<u64> = batch.events.iter().map(|event| event.offset).collect();
    assert_eq!(offsets, vec![1, 2]);
    let checkpoint = journal.checkpoint("orders", "a").unwrap();
    assert_eq!(checkpoint.offset, 1);
    assert_eq!(checkpoint.updated_at_ms, 10);

    let to_end = journal.seek_subscription_at("orders", "a", 3, 20).unwrap();
    assert_eq!((to_end.epoch, to_end.cursor), (2, 3));
    let same = journal.seek_subscription_at("orders", "a", 3, 30).unwrap();
    assert_eq!(same.epoch, 3);
}

#[test]
fn seek_out_of_range_is_refused_and_changes_nothing() {
    let mut journal = seeded_journal();
    journal.put_retention(
        "orders",
        RetentionPolicy {
            min_offset: Some(2),
            ..Default::default()
        },
        1000,
    );
    assert_eq!(
        journal.seek_subscription_at("orders", "a", 1, 0),
        Err(SubscriptionAckError::Checkpoint(
            TapeError::OffsetOutOfRange {
                offset: 1,
                earliest_offset: 2,
                end_offset: 3,
            }
        ))
    );
    assert_eq!(
        journal.seek_subscription_at("orders", "a", 4, 0),
        Err(SubscriptionAckError::Checkpoint(
            TapeError::OffsetOutOfRange {
                offset: 4,
                earliest_offset: 2,
                end_offset: 3,
            }
        ))
    );
    assert_eq!(journal.subscription("orders", "a").unwrap().epoch, 0);
    assert!(journal.checkpoint("orders", "a").is_none());
    assert!(journal.seek_subscription_at("orders", "a", 2, 0).is_ok());
    assert!(journal.seek_subscription_at("orders", "a", 3, 0).is_ok());
}

#[test]
fn pull_below_the_earliest_retained_offset_is_refused() {
    let mut journal = seeded_journal();
    journal.put_retention(
        "orders",
        RetentionPolicy {
            min_offset: Some(2),
            ..Default::default()
        },
        1000,
    );
    assert_eq!(
        journal.pull_subscription("orders", "a", None),
        Err(SubscriptionError::CursorOutOfRetention {
            cursor: 0,
            earliest_offset: 2,
            end_offset: 3,
        })
    );
    journal.seek_subscription_at("orders", "a", 2, 0).unwrap();
    let batch = journal.pull_subscription("orders", "a", None).unwrap();
    let offsets: Vec<u64> = batch.events.iter().map(|event| event.offset).collect();
    assert_eq!(offsets, vec![2]);
    assert_eq!(batch.epoch, 1);
}

#[test]
fn ack_with_a_stale_epoch_is_refused() {
    let mut journal = seeded_journal();
    let batch = journal.pull_subscription("orders", "a", None).unwrap();
    assert_eq!(batch.epoch, 0);
    journal.seek_subscription_at("orders", "a", 0, 0).unwrap();

    assert_eq!(
        journal.ack_subscription_epoch_at("orders", "a", 3, None, Some(0), 5),
        Err(SubscriptionAckError::Checkpoint(TapeError::StaleEpoch {
            epoch: 0,
            current_epoch: 1,
        }))
    );
    assert_eq!(journal.checkpoint("orders", "a").unwrap().offset, 0);

    let acked = journal
        .ack_subscription_epoch_at("orders", "a", 3, None, Some(1), 5)
        .unwrap();
    assert_eq!(acked.offset, 3);
    assert!(journal
        .ack_subscription_epoch_at("orders", "a", 3, None, None, 6)
        .is_ok());
}

#[test]
fn a_seek_on_one_subscription_leaves_the_others_alone() {
    let mut journal = seeded_journal();
    journal.create_subscription("orders", "b").unwrap();
    journal.put_checkpoint_at("orders", "b", 2, 0).unwrap();

    journal.seek_subscription_at("orders", "a", 1, 10).unwrap();

    assert_eq!(journal.subscription("orders", "b").unwrap().epoch, 0);
    assert_eq!(journal.checkpoint("orders", "b").unwrap().offset, 2);
    assert_eq!(journal.replay("orders", None, None, None).len(), 3);
}

#[test]
fn seek_epoch_survives_a_snapshot_round_trip() {
    let mut journal = seeded_journal();
    journal.seek_subscription_at("orders", "a", 1, 10).unwrap();

    let bytes = crate::snapshot::encode_snapshot(journal.clone(), 0).unwrap();
    let snapshot: crate::snapshot::JournalSnapshot = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        snapshot.journal.subscription("orders", "a").unwrap().epoch,
        1
    );
    assert_eq!(
        snapshot.journal.checkpoint("orders", "a").unwrap().offset,
        1
    );
}

#[test]
fn a_subscription_without_epoch_decodes_as_epoch_zero() {
    let subscription: Subscription = serde_json::from_str(r#"{"topic":"t","name":"n"}"#).unwrap();
    assert_eq!(subscription.epoch, 0);
}
