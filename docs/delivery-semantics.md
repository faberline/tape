# Tape Delivery Semantics & Producer Retry Contract

This document specifies the delivery semantics, retry contract, and deduplication guidance for producers and consumers integrating with Tape.

## At-Least-Once Append Contract

Event append in Tape is **at-least-once**.

When a producer sends an append request (`POST /topics/{topic}/append`, `src/server.rs:746-820`), Tape appends the event to the topic journal and assigns it a unique, monotonically increasing offset (`TapeJournal::append`, `src/lib.rs:163-175`).

If a network timeout occurs or Tape returns a retryable error response (such as HTTP `503 Service Unavailable`), the event may or may not have been durably written. If the producer retries the request, Tape appends the retried payload as a new event with a new, distinct offset. Tape does not collapse or deduplicate retried appends.

## Ambiguous Outcomes and Client-Side Control

Under Raft consensus, when an append proposal ages out or cannot be confirmed before timing out, Tape explicitly returns `503 Service Unavailable` (`src/server.rs:776-798`).

As documented in source code (`src/server.rs:776-778`), append is deliberately not idempotent:
> append is NOT idempotent (unlike a message_id-keyed publish), so an aged-out or failed outcome cannot be safely recomputed locally — surface 503 rather than silently re-appending a possible duplicate.

Tape surfaces `503` rather than attempting silent re-execution. This hands the retry decision—and the corresponding deduplication responsibility—explicitly to the client.

## Envelope `key` Behavior

The `AppendRequest` envelope accepts an optional `key` field (`src/server.rs:667-670`, `src/spec.rs:329-333`).

- **Opaque Metadata**: `key` is an opaque, caller-supplied label carried on the event envelope (`TapeEvent.key`) and returned on replay (`GET /topics/{topic}/replay`).
- **No Engine Inspection**: Tape's internal logic never inspects `key` for partitioning, ordering, routing, or deduplication. Tape has no partition model and performs no key-based indexing.
- **Unenforced Uniqueness**: Tape does not enforce uniqueness on `key`. Supplying a key does not make retries idempotent.

## Consumer-Side Deduplication Guidance

Because Tape guarantees at-least-once delivery without producer-side deduplication, applications requiring exactly-once processing must perform deduplication on the consumer side:

- **Payload-Based Identifiers**: Include a caller-generated unique identifier (such as a UUID or business transaction ID) **inside the event payload**. Downstream consumers should track processed identifiers within a local deduplication store or transactional database.
- **Envelope Key Usage**: Consumers may alternatively populate and use `TapeEvent.key` as a deduplication token if the producer guarantees its uniqueness, because `key` is preserved and returned verbatim on replay. However, Tape itself will not enforce this uniqueness.

## Tape Guarantees: Monotonic Offsets & Exact Replay

While Tape does not deduplicate producer appends, it provides strict ordering and replay guarantees:

- **Monotonic Offsets**: Offsets within a topic are strictly unique and monotonically increasing. Each append takes the topic's `next_offsets` counter and advances it by one (`src/lib.rs:163-175`).
- **Exact Replay**: Replaying a topic from an offset (`GET /topics/{topic}/replay`, `src/server.rs:828-839`) yields events in exact offset order.

A consumer that records its last successfully processed offset can reliably determine whether it has already processed a given event stream position.
