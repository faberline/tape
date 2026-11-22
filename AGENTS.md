# Tape repository instructions

Tape is Axiom's self-hosted stand-in for Google Cloud Pub/Sub: a Raft-replicated, durable topic service where publishers append messages to a topic and any number of subscriptions each receive every message.

## Output style

- Write in the user's language.
- Keep paths, commands and exact output intact.
- Use short, active sentences.
- Report what changed, what passed, what is unknown and the next action.

## Scope

- Keep the promises and work roots in `README.md`, the support state in `STATUS.md` and future outcomes in `ROADMAP.md` true.
- The repository is public. Put no secret, token, private host name, e-mail address or personal path in any file, issue or comment.

## Issue work

- Work on a claimed issue only through the `aw` CLI: `aw claim`, the work, then `aw report`.
- `aw` does the branch, push, pull request, merge and signal comment. Do not do them with `git` or `gh`.
- Edit labels only in the plan step of the `aw-worker` skill.
- The label table is in `CONTRIBUTING.md`.

## Verify

Run these commands before you report a change as ready. Each one is a `run:` line of `.github/workflows/ci.yml`:

- `cargo fmt --all -- --check` (job `lint: fmt, clippy`)
- `cargo clippy --workspace --all-targets --locked -- -D warnings` (job `lint: fmt, clippy`)
- `cargo test --locked --workspace` (job `test: workspace`)
- `cargo test --locked -p tape --features operator,backup` (job `test: tape (operator, backup)`)
- `uv run --python 3.13 --no-project scripts/meta/project_docs_contract.py check . --format json` (run it after editing `README.md`, `STATUS.md`, `ROADMAP.md` or `clients/README.md`)

Do not delete, skip, ignore or weaken a test, threshold, coverage ratchet or timeout. Functional and integration runs belong in CI (see `CONTRIBUTING.md`).
