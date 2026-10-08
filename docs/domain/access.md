# access

access decides who may call tape. Authentication, the role hierarchy, the
registry loader and the 401/403 shape all come from core's `service-auth`;
this context holds only tape's configuration names and its per-topic
authorization policy.

**Form:** application only · **Depends on:** — · **Source:** [`crates/tape-access/src/authorization.rs`](../../crates/tape-access/src/authorization.rs)

## Model

- **Auth mode** — `--auth` / `TAPE_AUTH`: `off` (the default; `disabled` is an
  alias) or `required`. Auth is all-or-nothing per deployment.
- **Token registry** — `--token-registry-file` / `TAPE_TOKEN_REGISTRY_FILE`:
  a JSON map from token to subject and per-topic roles. `TAPE_TOKENS` holds the
  same JSON inline, for development only.
- **Auth config** — `AuthConfig`: the mode and the loaded registry.
  `AuthConfig::resolve` fails at startup when auth is required and the registry
  is missing, unreadable or empty.
- **Resource** — the `{topic}` path parameter. A `*` grant covers every topic.
- **Role** — `read`, `write`, `admin`, where `admin` covers `write` covers
  `read`.

## Published language

- `AuthConfig` and its `verifier()`, which the `tape` assembly crate installs as the data-plane
  `auth_middleware`.
- `authorize(principal, topic, role)`, which every journal HTTP handler calls.

| Handler | Role |
|---------|------|
| append, subscription create and delete, retention put | `write` on the topic |
| replay, replay stream, checkpoint get and put, subscription list, get, pull and ack, retention get | `read` on the topic |
| `GET /admin/backup` | `admin` on `*` |

A checkpoint put moves only the caller's own cursor and appends no data, so it
needs `read`.

## Invariants

- The probe routes (`/healthz`, `/readyz`, `/metrics`, `/openapi.json`,
  `/docs`) never get the auth layer.
- This context never parses or logs a bearer token; audit events go through
  `service_auth::TracingAuthEventSink`. `crates/tape/tests/it/audit_contract.rs` checks
  the source for this.

## Exceptions and debts

None.
