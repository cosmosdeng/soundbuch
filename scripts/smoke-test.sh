#!/usr/bin/env bash
# End-to-end headless smoke test for soundbuch core on this machine.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

source "$HOME/.cargo/env" 2>/dev/null || true
CLI="target/debug/soundhub-cli"
if [[ ! -x "$CLI" ]]; then
  echo "Building soundhub-cli…"
  cargo build -p soundhub-cli
fi

WORK="${SOUNDHUB_SMOKE_DIR:-/tmp/soundhub-smoke}"
SAMPLES="$WORK/samples"
LIB="$WORK/Library"
rm -rf "$WORK"
mkdir -p "$WORK"

echo "══ 1. Generate sample audio ══"
python3 scripts/make-samples.py "$SAMPLES"

echo "══ 2. Create library ══"
"$CLI" init "$LIB"

echo "══ 3. Import samples ══"
"$CLI" import "$LIB" "$SAMPLES"

echo "══ 4. List assets ══"
"$CLI" list "$LIB"

echo "══ 5. Full-text search 'ocean' ══"
"$CLI" search "$LIB" ocean

echo "══ 6. Tag first asset + search by tag token ══"
FIRST=$("$CLI" list "$LIB" | head -1 | awk '{print $1}')
"$CLI" tag "$LIB" "$FIRST" seagull
"$CLI" search "$LIB" seagull

echo "══ 7. Batch-tag ══"
mapfile -t IDS < <("$CLI" list "$LIB" | awk '{print $1}')
"$CLI" batch-tag "$LIB" reviewed "${IDS[@]}"

echo "══ 8. Smart collection: sample_rate >= 48000 ══"
"$CLI" smart "$LIB" "HD" sample_rate gte 48000

echo "══ 9. GPS listing (empty expected — min WAV has no GPS) ══"
"$CLI" gps "$LIB"

echo "══ 10. Library info ══"
"$CLI" info "$LIB"

echo "══ 11. Recycle bin: soft-delete → trash → undo → restore → purge ══"
"$CLI" tag "$LIB" "$FIRST" tempmark
"$CLI" undo "$LIB"
"$CLI" delete "$LIB" "$FIRST"
"$CLI" trash "$LIB"
"$CLI" undo "$LIB"
"$CLI" list "$LIB" | head -3
"$CLI" delete "$LIB" "$FIRST"
"$CLI" restore "$LIB" "$FIRST"
"$CLI" delete "$LIB" "$FIRST"
"$CLI" purge "$LIB" "$FIRST"
"$CLI" trash "$LIB"
"$CLI" info "$LIB"

echo "══ 12. Duplicate detection + cleanup ══"
DUPDIR="$WORK/dups"
mkdir -p "$DUPDIR"
cp "$SAMPLES/ocean_wave.wav" "$DUPDIR/ocean_copy1.wav"
cp "$SAMPLES/ocean_wave.wav" "$DUPDIR/ocean_copy2.wav"
# Import as duplicate so the library holds multiple copies of one hash.
"$CLI" import --keep-duplicates "$LIB" "$DUPDIR"
"$CLI" dupes "$LIB"
"$CLI" dedupe "$LIB" oldest
"$CLI" dupes "$LIB" || true
"$CLI" undo "$LIB"
"$CLI" dupes "$LIB" | head -8
"$CLI" dedupe "$LIB" newest
"$CLI" info "$LIB"

echo "══ 13. Tag management + multi-tag filter ══"
"$CLI" tags "$LIB"
TAG_A=$("$CLI" tags "$LIB" | awk '$2=="reviewed"{print $1}')
TAG_B=$("$CLI" tags "$LIB" | awk '$2=="tempmark"{print $1}')
if [[ -n "$TAG_A" ]]; then
  echo "  — search-tags or reviewed tempmark —"
  "$CLI" search-tags "$LIB" or reviewed tempmark
  echo "  — search-tags and reviewed tempmark (expect 0) —"
  "$CLI" search-tags "$LIB" and reviewed tempmark
  "$CLI" tag-rename "$LIB" "$TAG_A" done
  "$CLI" search-tags "$LIB" or done
  if [[ -n "$TAG_B" ]]; then
    "$CLI" tag-merge "$LIB" "$TAG_B" "$TAG_A"
    "$CLI" tags "$LIB"
    "$CLI" undo "$LIB"
    "$CLI" tags "$LIB"
  fi
else
  echo "  (tags not found — skipped)"
fi

echo "══ 14. Playlists: create / add / reorder ══"
PL=$("$CLI" playlist "$LIB" create "DemoSet" | sed -n 's/.*(\([^)]*\)).*/\1/p')
echo "  created $PL"
# Grab first two live asset ids
mapfile -t LIVE < <("$CLI" list "$LIB" | awk '{print $1}' | head -2)
"$CLI" playlist-add "$LIB" "$PL" "${LIVE[@]}"
"$CLI" playlist "$LIB" show "$PL"
"$CLI" playlist-move "$LIB" "$PL" 0 1
echo "  after move 0→1:"
"$CLI" playlist "$LIB" show "$PL"
"$CLI" playlist-remove "$LIB" "$PL" "${LIVE[0]}"
echo "  after remove first:"
"$CLI" playlist "$LIB" show "$PL"
"$CLI" playlist "$LIB" list

echo ""
echo "✓ Smoke test finished. Library at: $LIB"
echo "  Launch the desktop app with:  scripts/run-local.sh"
