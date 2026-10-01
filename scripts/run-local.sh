#!/usr/bin/env bash
# Launch soundbuch desktop app (Linux).
# Linux/macOS development helper — not used by CI.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [[ ! -x target/debug/soundhub && ! -x target/release/soundhub ]]; then
  echo "Building soundbuch…"
  source "$HOME/.cargo/env" 2>/dev/null || true
  cargo build -p soundhub
fi

BIN="target/debug/soundhub"
[[ -x target/release/soundhub ]] && BIN="target/release/soundhub"

echo "Starting $BIN"
exec "$BIN"
