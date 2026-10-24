#!/usr/bin/env bash
# Produces the fixture set used by scripts/precision.sh, using am-hook's own
# EC-3 WASM for the core PCM (never FFmpeg):
#
#   scripts/make-fixtures.sh --input track.eac3 --joc-wasm joc.wasm --out DIR
#
# It writes the bed (interleaved float32 6/8 channels), the speaker and binaural
# renders produced by the same modules the browser worker uses, and a JSON summary.
# The EC-3 runtime/wasm default to an am-hook checkout (AM_HOOK=... or ../dev/am-hook).
set -eu
cd "$(dirname "$0")/.."
REPO=$(pwd)
AM_HOOK=${AM_HOOK:-$REPO/../dev/am-hook}
INPUT=""; OUT=""; JOC=""; FRAMES=""; MODE="speaker"
while [ $# -gt 0 ]; do
  case "$1" in
    --input) INPUT=$2; shift 2 ;;
    --joc-wasm) JOC=$2; shift 2 ;;
    --am-hook) AM_HOOK=$2; shift 2 ;;
    --out) OUT=$2; shift 2 ;;
    --frames) FRAMES=$2; shift 2 ;;
    --mode) MODE=$2; shift 2 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done
[ -n "$INPUT" ] && [ -n "$OUT" ] || { echo "usage: scripts/make-fixtures.sh --input x.eac3 --out DIR" >&2; exit 2; }
mkdir -p "$OUT"
if [ -z "$JOC" ]; then
  "$REPO/scripts/build-joc-wasm.sh" >/dev/null
  JOC=$REPO/joc-wasm/target/wasm32-unknown-unknown/wasm/joc_wasm.wasm
fi
node "$REPO/tests/joc_harness.mjs" \
  --ec3-runtime "$AM_HOOK/src/ui/ec3-runtime.mjs" \
  --ec3-wasm "$AM_HOOK/src/ui/ec3.wasm" \
  --joc-wasm "$JOC" \
  --input "$INPUT" --mode "$MODE" ${FRAMES:+--frames "$FRAMES"} \
  --out "$OUT/$MODE.f32" --bed-out "$OUT/core.f32le" --stats > "$OUT/harness-$MODE.json"
echo "fixtures written to $OUT"
