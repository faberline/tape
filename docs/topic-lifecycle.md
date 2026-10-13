# Tape Topic Lifecycle & Deletion Boundary

This document specifies what a topic is in Tape today: how it comes into existence, what retention removes and what it leaves behind, why there is no per-topic delete, and what an operator can do instead.

## Implicit Topic Creation

Topic creation in Tape is entirely implicit. There is no topic create, list, or delete route, and no CLI verb for any of them.

There is no topic record either. `TapeJournal` (`crates/tape-shared-kernel/src/tape_journal.rs`) keeps five maps keyed by topic name, and any write that names a topic adds to whichever map it touches:

- `POST /topics/{topic}/append` (`TapeJournal::append_at`) adds the event to `topics` and starts the topic's counter in `next_offsets`.
- `PUT /topics/{topic}/retention` (`TapeJournal::put_retention`) stores the policy in `retention` and, through `enforce_retention`, inserts an empty event list into `topics`.
- `POST /topics/{topic}/subscriptions` (`TapeJournal::create_subscription`) adds an entry to `subscriptions` only. It does not need the topic to have any events.
- `PUT /topics/{topic}/consumers/{consumer}/checkpoint` (`TapeJournal::put_checkpoint_at`) adds an entry to `checkpoints`.

No route answers 404 for an unknown topic. Replaying a topic that has never been written returns `200` with an empty `events` list, and a subscription can be created on a topic that has no events.

Topics declared on the custom resource (`TapeSpec.topics`, `crates/tape-operator/src/crd.rs`) follow the same rule: a declared topic with no subscriptions causes no journal mutation, and declared subscriptions are created at startup.

## Retention Bounds: Events Only

Retention policies set offset- or age-based bounds on a topic's events (`RetentionPolicy` in `crates/tape-shared-kernel/src/retention.rs`, applied by `TapeJournal::enforce_retention`).

Retention prunes **events only**. `enforce_retention` removes entries from the topic's event list and nothing else. After retention has pruned every event in a topic, this per-topic state remains in the journal indefinitely:

1. **The empty topic entry** in `topics`.
2. **The offset counter** in `next_offsets`. Offsets are never reused; the next append continues from where the topic left off.
3. **Every subscription** registered on the topic, in `subscriptions`.
4. **Every consumer checkpoint** recorded on the topic, in `checkpoints`, including checkpoints now behind the earliest retained offset.
5. **The retention policy** itself, in `retention`.

All five are carried in memory, in every snapshot, and in `GET /admin/backup`.

This was measured with `tape serve --data-dir` on a fresh directory: three appends to a topic, one subscription, a checkpoint at offset 2, then a retention policy with `min_offset: 3`. The retention call reported 3 events removed, replay returned no events, the subscription and the checkpoint at offset 2 were both still returned, and the next append was assigned offset 3.

## Deletion Boundary

There is no way to delete a single topic. `DELETE /topics/{topic}` does not exist and answers `404`. Explicit topic create, delete, and list routes are an open roadmap outcome (`resource-lifecycle-parity`, see [product/topics-and-publishing.md](product/topics-and-publishing.md)); until it ships, this section is the contract.

Whatever implements deletion has to go through Raft. In a replicated group every journal change is a `TapeCommand` (`crates/tape-shared-kernel/src/command.rs`) that each replica applies in log order. Deleting a topic needs a new command variant so that every replica removes the same state at the same point in the log.

A delete that changes one node's journal directly, without a Raft proposal, is a consistency hazard, not a lighter-weight alternative. A replica that misses the mutation, or catches up later from a snapshot or the log, would keep the topic that the other replicas dropped, and nothing in the log would reconcile them. The HA path for CR-provisioned subscriptions (`ensure_subscriptions` in `crates/tape/src/bin/tape/serve/provision.rs`) already mutates each replica's journal directly. It converges only because every replica runs the same deterministic, idempotent create at startup. A delete triggered on one node has no such symmetry.

The custom resource does not delete either. Topic declarations are additive-only: removing an entry from `TapeSpec.topics` never deletes journal data.

## Operator Workaround & Scope

No supported mechanism removes or purges an individual topic from a running Tape service.

Topic removal is only available for the whole service:

- **Service Retirement**: decommission the whole Tape custom resource and its data.
- **State Restoration**: restore the service from a backup taken before the topic was created, following the cold restore runbook in [deployment-handoff.md](deployment-handoff.md) section 7. Every write to any topic since that backup is lost.

A retention policy can empty a topic's events, which bounds its storage, but it leaves the five entries listed above in place.
