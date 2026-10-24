#!/usr/bin/env bash
# Builds joc.wasm from the Rust workspace.
#
#   scripts/build-joc-wasm.sh [--out FILE]
#
# Default output: joc-wasm/target/wasm32-unknown-unknown/wasm/joc.wasm
# The wasm profile (see joc-wasm/Cargo.toml) optimizes for size; wasm-opt is used
# when available (directly or through `npx -p binaryen`) and falls back to the
# unoptimized module when it cannot run.
set -eu
cd "$(dirname "$0")/.."
REPO=$(pwd)
OUT=${OUT:-$REPO/joc-wasm/target/wasm32-unknown-unknown/wasm/joc.wasm}

if ! rustup target list --installed | grep -qx wasm32-unknown-unknown; then
  echo "installing the wasm target" >&2
  rustup target add wasm32-unknown-unknown
fi

cd "$REPO/joc-wasm"
cargo build --profile wasm -p joc-wasm --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/wasm/joc_wasm.wasm "$OUT"

optimize() {
  wasm-opt -Oz --enable-bulk-memory "$1" -o "$1.opt" && mv "$1.opt" "$1"
}

if command -v wasm-opt >/dev/null 2>&1; then
  optimize "$OUT" || echo "warning: wasm-opt failed; keeping the unoptimized module" >&2
elif command -v npx >/dev/null 2>&1; then
  npx -y -p binaryen@123.0.0 wasm-opt -Oz --enable-bulk-memory "$OUT" -o "$OUT.opt" \
    && mv "$OUT.opt" "$OUT" \
    || echo "warning: wasm-opt via npx failed; keeping the unoptimized module" >&2
else
  echo "warning: wasm-opt not found; the output is unoptimized" >&2
fi

echo "joc.wasm -> $OUT ($(wc -c < "$OUT") bytes)"
