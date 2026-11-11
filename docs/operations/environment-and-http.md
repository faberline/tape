# Environment and HTTP surface

Copied from the retiring deployment handoff page (sections 4 and 5). Text unchanged.

## 4. Environment variables

`serve` reads flags and env (flag wins). Source: `crates/tape/src/bin/tape/serve.rs::ServeArgs`,
`crates/tape-access/src/authorization.rs`, core's `raft-runtime` (`src/cluster.rs`).

| Area | Env (≡ flag) | Default |
|------|--------------|---------|
| Bind | `TAPE_BIND` (`--bind`) | `127.0.0.1:7137` |
| Storage | `TAPE_STORE` (`--store`) | unset (in-memory) |
| Shutdown | `TAPE_GRACE_SECS` (`--grace-secs`) — the whole SIGTERM shutdown budget; set `terminationGracePeriodSeconds` to this plus 5 | `10` |
| Shutdown | `TAPE_DRAIN_DELAY_SECS` (`--drain-delay-secs`) — how long the node keeps serving after `/readyz` turns 503, capped by the grace budget | `5` |
| **Auth** | `TAPE_AUTH` (`--auth`) — `off`\|`required`; `TAPE_TOKEN_REGISTRY_FILE` (`--token-registry-file`) bearer-token registry JSON; `TAPE_TOKENS` legacy/dev inline fallback | `off` |
| **Raft HA** | `TAPE_DATA_DIR` (`--data-dir`), `TAPE_PEER_SERVICE` (`--peer-service`), `TAPE_PEERS`, plus the standard `POD_NAME`/`SHARD_COUNT`/`REPLICAS_PER_SHARD`/`VOTER_COUNT` downward-API quartet | peer-service `tape` |
| Peer mTLS | `TAPE_PEER_TLS_CERT`/`_KEY`/`_CA`, `TAPE_PEER_MTLS=on\|off` | unset (cleartext peer transport only when mTLS is off) |
| Backup | `TAPE_BACKUP_TOKEN` (`tape backup --token`) | unset |
| **ENOSPC recovery** | `TAPE_STORAGE_FULL_REPROBE_SECS` — how often a degraded node re-probes its store directory (#2573) | `30` |

> **Production:** set `TAPE_AUTH=required` + `TAPE_TOKEN_REGISTRY_FILE`, and
> for HA deployments set `REPLICAS_PER_SHARD>1` plus a durable `TAPE_DATA_DIR`
> volume. With `TAPE_PEER_MTLS=on`, complete peer material is validated before
> serving and the shared `raft-runtime` transport binds a dedicated required-mTLS
> raft listener. Public HTTP remains on the separate h2c + HTTP/1.1 port.


## 5. HTTP surface & probes

Registered in `crates/tape/src/http/router.rs` and `crates/tape-journal/src/interfaces/http.rs`
(standard probes via core's `service-http` shell; data-plane routes are
tape-specific):

| Path | Purpose | Auth |
|------|---------|------|
| `GET /healthz` | Liveness. | no |
| `GET /readyz` | Readiness — `503` from SIGTERM until the process exits. | no |
| `GET /metrics` | Prometheus text. | no |
| `GET /openapi.json`, `GET /docs` | OpenAPI 3 + Swagger UI. | no |
| `GET /version` | Build provenance: `version`, `git_sha`, `built_at`, `target`. | no |
| `GET /admin/backup` | Stream a whole-journal snapshot. | per `TAPE_AUTH` (needs `admin` role on `*` when `--auth required`) |
| `POST /topics/{topic}/append` | Append one message envelope; returns the assigned offset after the durable write. | per `TAPE_AUTH` (write) |
| `POST /topics/{topic}/subscriptions` | Create a named subscription on the topic. | per `TAPE_AUTH` (write) |
| `GET /topics/{topic}/subscriptions` | List the topic's subscriptions. | per `TAPE_AUTH` (read) |
| `GET /topics/{topic}/subscriptions/{subscription}` | Show one subscription. | per `TAPE_AUTH` (read) |
| `DELETE /topics/{topic}/subscriptions/{subscription}` | Delete a subscription. | per `TAPE_AUTH` (write) |
| `POST /topics/{topic}/subscriptions/{subscription}/pull` | Pull a bounded window from the subscription cursor. | per `TAPE_AUTH` (read) |
| `POST /topics/{topic}/subscriptions/{subscription}/ack` | Advance the subscription cursor cumulatively. | per `TAPE_AUTH` (write) |
| `PUT /topics/{topic}/retention` | Set the topic retention policy. | per `TAPE_AUTH` (write) |
| `GET /topics/{topic}/retention` | Read the topic retention policy. | per `TAPE_AUTH` (read) |
| `GET /topics/{topic}/replay` | Legacy: replay by offset or timestamp. Leaves with the seek outcome in `ROADMAP.md`. | per `TAPE_AUTH` (read) |
| `GET /topics/{topic}/replay/stream` | Legacy: compact read-only h2c bulk replay. Leaves with the seek outcome. | per `TAPE_AUTH` (read) |
| `PUT /topics/{topic}/consumers/{consumer}/checkpoint` | Legacy: advance a consumer cursor without a subscription. Leaves with the seek outcome. | per `TAPE_AUTH` (write) |
| `GET /topics/{topic}/consumers/{consumer}/checkpoint` | Legacy: read a consumer cursor. Leaves with the seek outcome. | per `TAPE_AUTH` (read) |

The served set equals `tape spec --format routes`; `cargo test -p tape --test it --
spec_route_parity::` refuses any drift between the router, the spec inventory,
and `clients/openapi.json`.

