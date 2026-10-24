#!/usr/bin/env bash
# Precision comparison against the shipped C++ reference (`joc_cli`).
#
#   scripts/precision.sh --input track.eac3 --bed core.f32le --hrtf field.jochrtf \
#       --joc-cli /path/to/joc_cli.exe [--frames N] [--out DIR]
#
# `core.f32le` must be the interleaved float32 core PCM of that stream *as decoded
# by am-hook's EC-3 WASM* (scripts/make-fixtures.sh produces it). The same bytes go
# to the Rust renderer and to joc_cli, so the comparison isolates the renderer:
#
#   1. native Rust CLI            -> f32
#   2. joc_cli with the same bed  -> WAV
#   3. tests/compare.py quantises both to int24 and reports LSB differences
#
# Both output modes are checked; the binaural run compares the programme part and
# the tail separately (joc_cli trims the tail at 1e-8).
set -eu
cd "$(dirname "$0")/.."
REPO=$(pwd)
INPUT=""; BED=""; HRTF=""; JOC_CLI=""; FRAMES=""; OUT=${OUT:-/tmp/joc-precision}
while [ $# -gt 0 ]; do
  case "$1" in
    --input) INPUT=$2; shift 2 ;;
    --bed) BED=$2; shift 2 ;;
    --hrtf) HRTF=$2; shift 2 ;;
    --joc-cli) JOC_CLI=$2; shift 2 ;;
    --frames) FRAMES=$2; shift 2 ;;
    --out) OUT=$2; shift 2 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done
[ -n "$INPUT" ] && [ -n "$BED" ] && [ -n "$JOC_CLI" ] || {
  echo "usage: scripts/precision.sh --input t.eac3 --bed core.f32le --hrtf f.jochrtf --joc-cli joc_cli.exe" >&2
  exit 2
}
mkdir -p "$OUT"

need_wasm() {
  if [ ! -f "$REPO/joc-wasm/target/wasm32-unknown-unknown/wasm/joc_wasm.wasm" ]; then
    "$REPO/scripts/build-joc-wasm.sh"
  fi
}
need_wasm
CLI=$REPO/joc-wasm/target/release/joc-cli.exe
if [ ! -f "$CLI" ]; then
  ( cd "$REPO/joc-wasm" && cargo build --release -p joc-cli )
fi

RENDER_ARGS=(--input "$INPUT" --bed "$BED")
[ -n "$HRTF" ] && RENDER_ARGS+=(--hrtf "$HRTF")
[ -n "$FRAMES" ] && RENDER_ARGS+=(--frames "$FRAMES")

status=0
for mode in speaker binaural; do
  if [ "$mode" = binaural ] && [ -z "$HRTF" ]; then
    echo "-- skipping binaural (no --hrtf)"
    continue
  fi
  echo "== $mode: reference =="
  if [ "$mode" = speaker ]; then
    "$JOC_CLI" --speaker-layout 2.0 --speaker-format float32 --bed "$BED" -o "$OUT/cli-$mode.wav" ${FRAMES:+--duration "$(python -c "print($FRAMES*1536/48000)")"} "$INPUT" >/dev/null
  else
    "$JOC_CLI" --binaural --binaural-format float32 --compiled-hrtf-cache "$HRTF" --bed "$BED" -o "$OUT/cli-$mode.wav" ${FRAMES:+--duration "$(python -c "print($FRAMES*1536/48000)")"} "$INPUT" >/dev/null
  fi
  echo "== $mode: Rust =="
  "$CLI" "${RENDER_ARGS[@]}" --mode "$mode" --out "$OUT/rust-$mode.f32" --stats 2>"$OUT/rust-$mode.log"
  echo "== $mode: compare =="
  # Binaural output is 961 samples shorter (the kernel's latency); compare that many.
  python "$REPO/tests/compare.py" --reference "$OUT/cli-$mode.wav" --candidate "$OUT/rust-$mode.f32" ${FRAMES:+--frames "$FRAMES"} || status=1
done
exit $status
