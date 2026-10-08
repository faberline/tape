# Glossary

Terms tape's code, API and docs use. Terms owned by one context are described
in more depth on its page under [domain/](domain/); architecture terms follow
core's [glossary](https://github.com/faberline/core/blob/main/docs/glossary.md).

## Journal

| Term | Meaning |
|------|---------|
| **topic** | A named, append-only sequence of events. Created by its first append. |
| **event** | One record in a topic: offset, timestamp, optional key, JSON payload (`TapeEvent`). |
| **offset** | An event's position in its topic, from 0, one per append. Never reused, even after retention removes the event. |
| **end offset** | The offset the next append will get. |
| **replay** | Reading a topic's events oldest-first from an offset or timestamp. Bounded to 1000 events per call. |
| **replay stream** | The same read in the compact binary format `application/vnd.tape.replay.v1`. |
| **consumer** | A named reader that stores its own checkpoint. |
| **checkpoint** | The offset a consumer has processed up to in a topic. Moves forward only and never past the end offset. |
| **subscription** | A named pull cursor on a topic, stored as a checkpoint with the subscription's name. |
| **pull** | Reading the next window from a subscription's cursor. Has no side effect. |
| **ack** | Advancing a subscription's cursor after processing a pulled window. |
| **retention policy** | Per-topic rules for removing old events: a minimum offset, a maximum age, and protected consumers. |
| **protected consumer** | A consumer whose checkpoint retention never removes past. |
| **command** | One write (`TapeCommand`). The unit that crosses the WAL and the raft log. |
| **outcome** | The local result of applying one command (`TapeOutcome`). Never replicated. |
| **snapshot** | The whole journal at a raft applied index (`JournalSnapshot`). Also the backup and bootstrap-seed format. |
| **degraded mode** | Read-only mode after a disk-full or I/O durability failure, until the storage re-probe clears it. |

## Durability

| Term | Meaning |
|------|---------|
| **commit log** | The single-node durability port (`CommitLog`). |
| **WAL** | The group-commit write-ahead log under `--data-dir`. Logs commands. |
| **legacy store** | The whole-file journal under `--store`, rewritten on every write. |
| **replicator** | The replicated durability port (`Replicator`), implemented by the raft group. |
| **raft group** | The set of tape replicas that replicate every topic through one `raft-runtime` host. tape has one group and no shards. |
| **bootstrap seed** | A snapshot a new group restores from before its first election (`--bootstrap-seed-uri`). |

## Access

| Term | Meaning |
|------|---------|
| **token registry** | The JSON map from bearer token to subject and per-topic roles. |
| **role** | `read`, `write` or `admin` on a topic or on `*`; each covers the ones before it. |

## Architecture

| Term | Meaning |
|------|---------|
| **context** | A bounded context: `journal`, `storage`, `replication`, `access` or `operator`, each one crate. See [architecture](architecture.md#crates). |
| **shared kernel** | `tape-shared-kernel`: the pure journal model and durability ports every context may use. |
| **assembly** | The `tape` and `tape-bench` crates: code that wires contexts together into binaries and belongs to no layer. |
| **core** | The faberline/core repository, whose crates tape depends on by git tag. |
