# Tape Topic Administrative Lifecycle & Deletion Boundary

This document specifies Tape's topic-level administrative lifecycle: implicit creation, the scope of retention pruning, the documented deletion boundary, and operator administrative procedures.

## Implicit Topic Creation

Topic creation in Tape is entirely implicit. There is no explicit topic creation route or CLI verb.

A topic springs into existence on its first write operation (`TapeJournal::append_at`, `src/lib.rs:175-178`) or when a consumer registers a subscription on it (`TapeJournal::create_subscription`, `src/lib.rs:284-298`). The journal automatically initializes the topic entry in its internal map upon receiving either request.

## Retention Bounds: Events Only

Retention policies configure age- or offset-based bounds for event log pruning (`TapeJournal::retention`, `put_retention`, `enforce_retention`, `src/lib.rs:416-480`).

However, retention prunes **events only**. Calling `enforce_retention` (`src/lib.rs:443-476`) truncates event entries within the topic log map (`src/lib.rs:474`), but it does not remove topic keys or alter subscription state. Consequently, after retention has pruned all eligible events, three entities persist in journal state indefinitely:

1. **The empty topic shell**: The topic key remains present in `TapeJournal.topics`.
2. **Subscriptions**: Every `Subscription` entry registered for the topic persists in `TapeJournal.subscriptions`.
3. **Checkpoints**: Every consumer `Checkpoint` recorded for the topic persists in `TapeJournal.checkpoints`.

Stating merely that "the topic persists" undersells the actual state footprint; all three components remain permanently in memory and disk storage.

## Deletion Boundary: Documented Non-Goal

Topic deletion (`DELETE /topics/{topic}`) is an explicit **documented non-goal** for Tape.

The issue-proposed deletion mechanism (`DELETE /topics/{topic}` with drain semantics and a Raft-replicated tombstone) requires introducing a new `TapeCommand` variant in `src/raft.rs`. Mutating Raft state machine commands is out of bounds for single-node / surface campaigns.

Furthermore, a local, single-node-only deletion route (bypassing Raft consensus to mutate local journal state directly) is explicitly rejected. As established in the precedent from `#2568`/`#3284`, direct local journal mutations—such as `ensure_subscriptions` bypassing Raft proposal—"converges today only because every replica performs an identical deterministic mutation." A single-node topic delete exhibits the exact same failure pattern, causing replica divergence during rolling operations or snapshot catch-ups when Raft log replication is bypassed. A single-node deletion is therefore a named consistency hazard, not a viable partial workaround.

Declarative topic manifests also reflect this boundary (`src/operator/crd.rs:147-148`), noting that topic declarations are additive-only and removing an entry from `TapeSpec.topics` never deletes journal data.

## Operator Workaround & Scope

No supported mechanism exists to purge or delete an individual topic from a running Tape service instance.

Granularity for topic removal is restricted to the entire service instance:
- **Service Retirement**: Decommission or abstain from re-provisioning the entire Tape custom resource (`TapeSpec`).
- **State Restoration**: Restore the Tape cluster from a backup taken prior to the creation of the target topic.

No lighter-weight or per-topic purge workaround is available.
