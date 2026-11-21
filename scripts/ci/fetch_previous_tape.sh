#!/usr/bin/env bash
# Copy the `tape` binary of the pinned previous release image to a file.
# Usage: fetch_previous_tape.sh <output-path>
# The image is named in .github/previous-release-image (one line).
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: fetch_previous_tape.sh <output-path>" >&2
  exit 2
fi
out="$1"

cd "$(dirname "$0")/../.."
image="$(head -n 1 .github/previous-release-image)"
if ! printf '%s\n' "$image" | grep -Eq '^ghcr\.io/faberline/tape@sha256:[0-9a-f]{64}$'; then
  echo "fetch_previous_tape: .github/previous-release-image must be ghcr.io/faberline/tape@sha256:<64 hex>" >&2
  exit 1
fi

docker pull --platform linux/amd64 "$image"
cid="$(docker create --platform linux/amd64 "$image")"
trap 'docker rm "$cid" >/dev/null' EXIT
docker cp "$cid:/usr/local/bin/tape" "$out"
chmod +x "$out"
"$out" --version
