use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use raft_runtime::RaftStateMachine;

use super::state_machine::snapshot_path_for;
use super::*;
use tape_shared_kernel::{JournalSnapshot, TapeError, TapeOutcome};

fn journal() -> Arc<Mutex<TapeJournal>> {
    Arc::new(Mutex::new(TapeJournal::default()))
}

#[test]
fn apply_append_stashes_claimable_outcome() {
    let sm = TapeStateMachine::new(journal(), None).unwrap();
    let cmd = TapeCommand::Append {
        topic: "orders".into(),
        key: None,
        payload: serde_json::json!({"n": 1}),
        timestamp_ms: 100,
        applied_at_ms: 100,
    };
    sm.apply(1, &serde_json::to_vec(&cmd).unwrap()).unwrap();
    assert_eq!(sm.applied_index(), 1);
    let outcome = sm.claim_outcome(1).expect("outcome stashed");
    match outcome {
        TapeOutcome::Appended(event) => {
            assert_eq!(event.topic, "orders");
            assert_eq!(event.offset, 0);
            assert_eq!(event.timestamp_ms, 100);
        }
        _ => panic!("expected Appended outcome"),
    }
    // Claiming twice returns None.
    assert!(sm.claim_outcome(1).is_none());
}

#[test]
fn apply_checkpoint_put_surfaces_stale_rejection() {
    let j = journal();
    j.lock()
        .unwrap()
        .append_at("orders", None, serde_json::json!({"n": 1}), 100, 100);
    let sm = TapeStateMachine::new(j, None).unwrap();
    let cmd = TapeCommand::CheckpointPut {
        topic: "orders".into(),
        consumer: "c1".into(),
        offset: 5,
        updated_at_ms: 200,
    };
    sm.apply(1, &serde_json::to_vec(&cmd).unwrap()).unwrap();
    match sm.claim_outcome(1).expect("outcome") {
        TapeOutcome::Checkpoint(Err(TapeError::CheckpointBeyondEnd { .. })) => {}
        other => panic!("expected CheckpointBeyondEnd, got {other:?}"),
    }
}

#[test]
fn snapshot_restore_round_trips_whole_journal() {
    let j = journal();
    j.lock()
        .unwrap()
        .append_at("orders", None, serde_json::json!({"n": 1}), 100, 100);
    let sm = TapeStateMachine::new(j, None).unwrap();
    sm.applied.store(3, Ordering::Release);
    let mut bytes = Vec::new();
    sm.snapshot(&mut bytes).unwrap();

    let fresh = TapeStateMachine::new(journal(), None).unwrap();
    fresh.restore(&mut std::io::Cursor::new(&bytes)).unwrap();
    assert_eq!(fresh.applied_index(), 3);
    assert_eq!(fresh.journal().lock().unwrap().end_offset("orders"), 1);
}

#[test]
fn legacy_snapshot_floor_is_still_read_during_migration() {
    let dir = std::env::temp_dir().join(format!("tape-sm-marker-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let marker = dir.join("applied-0.idx");
    let legacy = journal();
    legacy
        .lock()
        .unwrap()
        .append_at("orders", None, serde_json::json!({"n": 1}), 100, 100);
    let bytes = serde_json::to_vec(&JournalSnapshot {
        up_to: 1,
        journal: legacy.lock().unwrap().clone(),
        completed_proposals: Vec::new(),
    })
    .unwrap();
    std::fs::write(snapshot_path_for(&marker), bytes).unwrap();
    std::fs::write(&marker, b"1").unwrap();
    let sm2 = TapeStateMachine::new(journal(), Some(marker.clone())).unwrap();
    assert_eq!(sm2.applied_index(), 1);
    assert_eq!(sm2.journal().lock().unwrap().end_offset("orders"), 1);

    let cmd = TapeCommand::Append {
        topic: "orders".into(),
        key: None,
        payload: serde_json::json!({"n": 1}),
        timestamp_ms: 100,
        applied_at_ms: 100,
    };
    sm2.apply(1, &serde_json::to_vec(&cmd).unwrap()).unwrap();
    // Skipped: re-applying the same (already-recovered) index does not
    // double the entry.
    assert_eq!(sm2.journal().lock().unwrap().end_offset("orders"), 1);

    std::fs::remove_dir_all(&dir).ok();
}
