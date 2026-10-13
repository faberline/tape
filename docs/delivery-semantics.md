# Tape Delivery Semantics & Producer Retry Contract

This document specifies the delivery semantics, retry contract, and deduplication guidance for producers and consumers integrating with Tape.

## At-Least-Once Append Contract

Event append in Tape is **at-least-once**.

When a producer sends an append request (`POST /topics/{topic}/append`, `append` in `crates/tape-journal/src/interfaces/http/topics.rs`), Tape appends the event to the topic journal and assigns it a unique, monotonically increasing offset (`TapeJournal::append_at`, `crates/tape-shared-kernel/src/tape_journal.rs`).

If a network timeout occurs or Tape returns a retryable error response (such as HTTP `503 Service Unavailable`), the event may or may not have been durably written. If the producer retries the request, Tape appends the retried payload as a new event with a new, distinct offset. Tape does not collapse or deduplicate retried appends.

## Ambiguous Outcomes and Client-Side Control

Under Raft consensus, when an append proposal fails or its outcome ages out before the node can read it back, Tape explicitly returns `503 Service Unavailable` with code `raft_unavailable` (`JournalService::mutate` in `crates/tape-journal/src/application/service/mutations.rs`, mapped in `crates/tape-journal/src/interfaces/http/errors.rs`).

As documented in source code (`JournalService::append`), append is deliberately not idempotent:
> Append is NOT idempotent (unlike a message_id-keyed publish), so an aged-out replicated outcome is surfaced as unavailable rather than silently re-appended.

Tape surfaces `503` rather than attempting silent re-execution. This hands the retry decision—and the corresponding deduplication responsibility—explicitly to the client.

The raft proposal-outcome cache does not change this. It is keyed by a `ProposalId { node, session, sequence }` minted fresh on every proposal, so it gives exactly-once *apply* of one committed entry; a client retry mints a new id and commits a second entry. The client's `key` never reaches it.

## Envelope `key` Behavior

The append request accepts an optional `key` field (`AppendRequest` in `crates/tape-journal/src/interfaces/http/dto.rs`, `AppendEventRequest` in the OpenAPI document).

- **Opaque Metadata**: `key` is an opaque, caller-supplied label carried on the event envelope (`TapeEvent.key`) and returned on replay (`GET /topics/{topic}/replay`).
- **No Engine Inspection**: Tape's internal logic never inspects `key` for partitioning, ordering, routing, or deduplication. Tape has no partition model and performs no key-based indexing.
- **Unenforced Uniqueness**: Tape does not enforce uniqueness on `key`. Supplying a key does not make retries idempotent.

## Consumer-Side Deduplication Guidance

Because Tape guarantees at-least-once delivery without producer-side deduplication, applications requiring exactly-once processing must perform deduplication on the consumer side:

- **Payload-Based Identifiers**: Include a caller-generated unique identifier (such as a UUID or business transaction ID) **inside the event payload**. Downstream consumers should track processed identifiers within a local deduplication store or transactional database.
- **Envelope Key Usage**: Consumers may alternatively populate and use `TapeEvent.key` as a deduplication token if the producer guarantees its uniqueness, because `key` is preserved and returned verbatim on replay. However, Tape itself will not enforce this uniqueness.

## Tape Guarantees: Monotonic Offsets & Exact Replay

While Tape does not deduplicate producer appends, it provides strict ordering and replay guarantees:

- **Monotonic Offsets**: Offsets within a topic are strictly unique and monotonically increasing. Each append takes the topic's `next_offsets` counter and advances it by one (`TapeJournal::append_at`).
- **Exact Replay**: Replaying a topic from an offset (`GET /topics/{topic}/replay`, `replay` in `crates/tape-journal/src/interfaces/http/topics.rs`) yields events in exact offset order.

A consumer that records its last successfully processed offset can reliably determine whether it has already processed a given event stream position.

`crates/tape/tests/it/producer_at_least_once.rs` pins this contract: two appends with the same key and payload get distinct offsets, and replay returns both with the key intact.
