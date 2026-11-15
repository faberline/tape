//! Property tests for two untrusted-input and state boundaries: the JSON
//! request parsers of the data plane never panic and keep every valid field,
//! and the pull-subscription cursor never moves back or passes the latest
//! offset of its topic.

use proptest::prelude::*;
use serde_json::{json, Value};
use tape_journal::interfaces::http::{
    AppendRequest, CheckpointPutRequest, SubscriptionAckRequest, SubscriptionCreateRequest,
    SubscriptionPullRequest,
};
use tape_shared_kernel::{
    SubscriptionAckError, SubscriptionError, TapeError, TapeJournal, MAX_PULL_BATCH,
};

/// Runs every request parser on `bytes` and counts how many accept them.
fn parse_all(bytes: &[u8]) -> usize {
    [
        serde_json::from_slice::<AppendRequest>(bytes).is_ok(),
        serde_json::from_slice::<SubscriptionCreateRequest>(bytes).is_ok(),
        serde_json::from_slice::<SubscriptionPullRequest>(bytes).is_ok(),
        serde_json::from_slice::<SubscriptionAckRequest>(bytes).is_ok(),
        serde_json::from_slice::<CheckpointPutRequest>(bytes).is_ok(),
    ]
    .iter()
    .filter(|ok| **ok)
    .count()
}

fn json_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::from),
        any::<i64>().prop_map(Value::from),
        "\\PC{0,16}".prop_map(Value::from),
    ];
    leaf.prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            prop::collection::btree_map("\\PC{0,8}", inner, 0..4)
                .prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}

#[test]
fn parse_all_counts_good_bodies_and_refuses_garbage() {
    // Only the append body and the pull body (`limit` defaults, and the
    // unknown `payload` key is ignored) are accepted.
    assert_eq!(parse_all(br#"{"payload":{}}"#), 2);
    assert_eq!(parse_all(b"\xff\xfe\x00"), 0);
    assert_eq!(parse_all(b""), 0);
}

proptest! {
    #![proptest_config(ProptestConfig { failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn arbitrary_bytes_never_panic_any_request_parser(
        bytes in prop::collection::vec(any::<u8>(), 0..512),
    ) {
        let _ = parse_all(&bytes);
    }

    #[test]
    fn arbitrary_json_values_never_panic_any_request_parser(value in json_value()) {
        let bytes = serde_json::to_vec(&value).unwrap();
        let _ = parse_all(&bytes);
    }

    #[test]
    fn append_request_round_trips_key_payload_and_timestamp(
        key in prop::option::of("\\PC{0,24}"),
        payload in json_value(),
        timestamp_ms in prop::option::of(any::<u64>()),
    ) {
        let mut body = serde_json::Map::new();
        body.insert("payload".into(), payload.clone());
        if let Some(k) = &key {
            body.insert("key".into(), json!(k));
        }
        if let Some(t) = timestamp_ms {
            body.insert("timestamp_ms".into(), json!(t));
        }
        let bytes = serde_json::to_vec(&Value::Object(body)).unwrap();
        let req: AppendRequest = serde_json::from_slice(&bytes).unwrap();
        prop_assert_eq!(req.key, key);
        prop_assert_eq!(req.payload, payload);
        prop_assert_eq!(req.timestamp_ms, timestamp_ms);
    }

    #[test]
    fn subscription_requests_round_trip(
        name in "\\PC{0,24}",
        offset in any::<u64>(),
        limit in prop::option::of(0usize..100_000),
    ) {
        let created: SubscriptionCreateRequest =
            serde_json::from_value(json!({ "name": name.clone() })).unwrap();
        prop_assert_eq!(created.name, name);

        let ack: SubscriptionAckRequest =
            serde_json::from_value(json!({ "offset": offset })).unwrap();
        prop_assert_eq!(ack.offset, offset);

        let checkpoint: CheckpointPutRequest =
            serde_json::from_value(json!({ "offset": offset })).unwrap();
        prop_assert_eq!(checkpoint.offset, offset);

        let body = match limit {
            Some(n) => json!({ "limit": n }),
            None => json!({}),
        };
        let pull: SubscriptionPullRequest = serde_json::from_value(body).unwrap();
        prop_assert_eq!(pull.limit, limit);
    }
}

const SUBS: [&str; 2] = ["a", "b"];

#[derive(Clone, Debug)]
enum Op {
    Append,
    Ack { sub: usize, offset: u64 },
    Pull { sub: usize, limit: usize },
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => Just(Op::Append),
        3 => (0usize..2, 0u64..40).prop_map(|(sub, offset)| Op::Ack { sub, offset }),
        2 => (0usize..2, 0usize..=(MAX_PULL_BATCH + 5))
            .prop_map(|(sub, limit)| Op::Pull { sub, limit }),
    ]
}

fn ops() -> impl Strategy<Value = Vec<Op>> {
    prop::collection::vec(op(), 0..60)
}

fn fresh_journal() -> TapeJournal {
    let mut journal = TapeJournal::default();
    for sub in SUBS {
        journal.create_subscription("t", sub).unwrap();
    }
    journal
}

fn cursor(journal: &TapeJournal, sub: &str) -> u64 {
    journal.checkpoint("t", sub).map_or(0, |c| c.offset)
}

/// Checks the cursor of `sub` against its previous value and the topic's
/// latest offset, and returns the cursor.
fn assert_cursor_invariants(journal: &TapeJournal, sub: &str, previous: u64) -> u64 {
    let cursor = cursor(journal, sub);
    assert!(
        cursor >= previous,
        "cursor moved back: {sub} {previous} -> {cursor}"
    );
    assert!(
        cursor <= journal.end_offset("t"),
        "cursor passed the latest offset: {sub} {cursor}"
    );
    cursor
}

#[test]
#[should_panic(expected = "cursor moved back")]
fn cursor_invariant_helper_panics_when_the_cursor_moved_back() {
    let mut journal = fresh_journal();
    for i in 0..3u64 {
        journal.append_at("t", None, json!({ "n": i }), i, i);
    }
    journal.ack_subscription_at("t", "a", 2, 0).unwrap();
    assert_cursor_invariants(&journal, "a", 5);
}

proptest! {
    #![proptest_config(ProptestConfig { failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn cursor_never_moves_back_or_passes_the_latest_offset(ops in ops()) {
        let mut journal = fresh_journal();
        let mut previous = [0u64; 2];

        for (index, op) in ops.iter().enumerate() {
            match *op {
                Op::Append => {
                    let i = index as u64;
                    journal.append_at("t", None, json!({ "n": i }), i, i);
                }
                Op::Ack { sub, offset } => {
                    let name = SUBS[sub];
                    let cur = cursor(&journal, name);
                    let end = journal.end_offset("t");
                    let before = journal.clone();
                    let result = journal.ack_subscription_at("t", name, offset, 0);
                    if offset > end {
                        let beyond = matches!(
                            result,
                            Err(SubscriptionAckError::Checkpoint(
                                TapeError::CheckpointBeyondEnd { .. }
                            ))
                        );
                        prop_assert!(beyond, "ack beyond the end must be refused");
                        prop_assert_eq!(&journal, &before);
                    } else if offset < cur {
                        let stale = matches!(
                            result,
                            Err(SubscriptionAckError::Checkpoint(
                                TapeError::StaleCheckpoint { .. }
                            ))
                        );
                        prop_assert!(stale, "ack below the cursor must be refused");
                        prop_assert_eq!(&journal, &before);
                    } else {
                        prop_assert!(result.is_ok());
                        prop_assert_eq!(cursor(&journal, name), offset);
                    }
                }
                Op::Pull { sub, limit } => {
                    let name = SUBS[sub];
                    let cur = cursor(&journal, name);
                    let before = journal.clone();
                    let result = journal.pull_subscription("t", name, Some(limit));
                    prop_assert_eq!(&journal, &before);
                    if limit > MAX_PULL_BATCH {
                        let too_large = matches!(
                            result,
                            Err(SubscriptionError::PullBatchTooLarge { .. })
                        );
                        prop_assert!(too_large, "a pull above the batch limit must be refused");
                    } else {
                        prop_assert!(result.is_ok());
                        let batch = result.expect("pull within the batch limit");
                        prop_assert_eq!(batch.cursor, cur);
                        prop_assert!(batch.events.len() <= limit);
                        for (i, event) in batch.events.iter().enumerate() {
                            prop_assert_eq!(event.offset, cur + i as u64);
                        }
                        prop_assert!(batch.next_offset <= journal.end_offset("t"));
                    }
                }
            }

            for (k, sub) in SUBS.iter().enumerate() {
                previous[k] = assert_cursor_invariants(&journal, sub, previous[k]);
            }
        }
    }
}

#[test]
fn a_backward_or_beyond_end_ack_keeps_the_cursor() {
    let mut journal = fresh_journal();
    for i in 0..3u64 {
        journal.append_at("t", None, json!({ "n": i }), i, i);
    }
    assert!(journal.ack_subscription_at("t", "a", 2, 0).is_ok());
    assert!(matches!(
        journal.ack_subscription_at("t", "a", 1, 0),
        Err(SubscriptionAckError::Checkpoint(
            TapeError::StaleCheckpoint {
                current_offset: 2,
                new_offset: 1,
            }
        ))
    ));
    assert!(matches!(
        journal.ack_subscription_at("t", "a", 4, 0),
        Err(SubscriptionAckError::Checkpoint(
            TapeError::CheckpointBeyondEnd {
                offset: 4,
                end_offset: 3,
            }
        ))
    ));
    assert_eq!(cursor(&journal, "a"), 2);
}
