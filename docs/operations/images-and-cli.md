# Images and CLI

Copied from the retiring deployment handoff page (sections 1 and 2). Text unchanged.

## 1. Image / binary

| Path | Use | Runtime base |
|------|-----|--------------|
| `Dockerfile.release` | **Production-like.** Fetches the published release binary — no Rust toolchain, smallest attack surface. | distroless |
| `Dockerfile` | Dev/CI build from source (`cargo build --release -p tape`). | distroless |
| `Dockerfile.test` | **Internal only (#2576).** Same runtime stage as `Dockerfile.release`, but COPYs a locally built binary — the only way to get an *unreleased* commit into a cluster. | distroless |

```bash
tape dockerfile render --variant release --version 0.1.0 --out /tmp/tape-image
docker build -f /tmp/tape-image/Dockerfile.release -t tape:0.1.0 .
docker run --rm -p 7137:7137 tape:0.1.0 serve
```

`tape dockerfile render --variant source` renders the source-build Dockerfile
for local/CI images. Raft peer RPCs share the same h2c HTTP port as the
serving API (no separate peer port).

### Two published image lines

| Line | Tag | Produced by | Audience |
|------|-----|-------------|----------|
| release | `ghcr.io/faberline/tape:<semver>` + `latest` | `.github/workflows/tape-release.yml` | integrators — the only line to hand a user |
| dev/test | `ghcr.io/faberline/tape:sha-<git12>` | `.github/workflows/tape-test-image.yml` | acceptance harnesses only |

The dev/test line exists so that verifying a commit in a real cluster does
not require cutting a release first — that is how tape went 0.4.5 → 0.4.11
during the acceptance campaign, seven version bumps carrying no user-facing
change. Dispatch `tape-test-image` (optionally naming a `ref`); its run
summary prints the pushed tag already pinned by digest, ready to paste into
the acceptance harness's `TAPE_IMAGE`. Both lines land on the same GHCR
package, so `sha-*` tags inherit its public visibility and GKE pulls them
without a pull secret. `tape-test-image.yml` must never add `latest` or a
semver tag.


## 2. CLI surface (first-level commands)

`tape --help` (from `crates/tape/src/bin/tape/cli.rs`):

| Command | Purpose |
|---------|---------|
| `tape serve` | Run a serving node: HTTP API + (in HA mode) the raft group. |
| `tape append` / `tape replay` / `tape checkpoint` | Local file-backed journal admin (no server needed). |
| `tape spec` / `tape spec gen` | Print the machine-readable contract, or generate a typed ts/py/rust client. |
| `tape backup` | Pull a snapshot from a running node's `/admin/backup` and ship it to a destination sink (#1329). |
| `tape dockerfile` | Render source/release Dockerfiles. |
| `tape k8s` | Render Kubernetes layers: `crd render`, `operator render\|run`, `instance render`. |
| `tape llm` / `tape upgrade` / `tape issue` | Shared ecosystem CLI convention (agent docs, self-update, issue search/view/create). |

