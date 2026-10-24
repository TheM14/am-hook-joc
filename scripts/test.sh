#!/usr/bin/env bash
# Runs the reproducible checks that do not need an Apple Music account:
#
#   scripts/test.sh [--server http://127.0.0.1:8888] [--fixtures DIR]
#
# * Rust unit tests (cargo test -p joc-core)
# * patch application against the locked release (git apply --check)
# * optional, with --fixtures: the precision comparison against joc_cli and the
#   seek-state measurement
# * optional, with --server: the browser end-to-end checks (Playwright)
set -eu
cd "$(dirname "$0")/.."
REPO=$(pwd)
SERVER=""
FIXTURES=""
while [ $# -gt 0 ]; do
  case "$1" in
    --server) SERVER=$2; shift 2 ;;
    --fixtures) FIXTURES=$2; shift 2 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done

echo "== Rust unit tests =="
( cd "$REPO/joc-wasm" && cargo test -p joc-core )

TAG_CHECK=${TAG:-v0.5.9}
echo "== patch applies to $TAG_CHECK =="
if [ -d "${UPSTREAM:-}" ]; then
  "$REPO/scripts/apply.sh" "$UPSTREAM" --check
else
  echo "skipped (set UPSTREAM=/path/to/clean/am-hook to check)"
fi

if [ -n "$FIXTURES" ]; then
  echo "== precision against joc_cli =="
  "$REPO/scripts/precision.sh"     --input "$FIXTURES/reference.eac3"     --bed "$FIXTURES/core.f32le"     --hrtf "$FIXTURES/field.jochrtf"     --joc-cli "${JOC_CLI:-joc_cli.exe}"     --out "$FIXTURES/out"
fi

if [ -n "$SERVER" ]; then
  echo "== browser end-to-end =="
  : "${TRACK:?set TRACK to a track id with an Atmos variant}"
  : "${TRACK2:?set TRACK2 to a second track id with an Atmos variant}"
  ( cd "$REPO/tests" && npm install --no-audit --no-fund >/dev/null 2>&1 || true
    node e2e_joc.mjs --base "$SERVER" --track "${TRACK}" --mode speaker --gain -4 --seconds 5
    node e2e_joc.mjs --base "$SERVER" --track "${TRACK}" --mode binaural --gain -4 --seconds 5
    node regression_check.mjs "$SERVER" "${TRACK}"
    node fallback_check.mjs "$SERVER" "${TRACK}"
    node queue_check.mjs "$SERVER" "${TRACK}" "${TRACK2}" speaker
    node settings_check.mjs "$SERVER" "${OUT_DIR:-/tmp}"
    node settings_ui_check.mjs "$SERVER"
    node dataset_check.mjs "$SERVER" "$TRACK"
  )
fi
echo "done"
