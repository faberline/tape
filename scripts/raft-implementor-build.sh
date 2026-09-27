#!/usr/bin/env bash
# Slow migration gate: compiles every registered RaftStateMachine implementor
# in this repository. Every other application's implementors build in its own
# faberline repository, and raft-runtime's implementor_build_coverage inventory
# lives with raft-runtime in faberline/core.
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

echo "cargo build -p tape"
cargo build -p tape
