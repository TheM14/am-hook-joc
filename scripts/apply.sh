#!/usr/bin/env bash
# Applies the JOC patch to a clean am-hook checkout of the locked release.
#
#   scripts/apply.sh /path/to/am-hook [--tag v0.5.9] [--check]
#
# The patch only touches the web front end (assets.rs, player.js, prefs.mjs,
# i18n.js, the settings view and three new UI files). It contains no binary: the
# wasm is built by scripts/build-joc-wasm.sh.
set -eu
cd "$(dirname "$0")/.."
REPO=$(pwd)
TAG=v0.5.9
CHECK_ONLY=0
DIR=""
while [ $# -gt 0 ]; do
  case "$1" in
    --tag) TAG=$2; shift 2 ;;
    --check) CHECK_ONLY=1; shift ;;
    -*) echo "unknown option $1" >&2; exit 2 ;;
    *) DIR=$1; shift ;;
  esac
done
if [ -z "$DIR" ]; then echo "usage: scripts/apply.sh /path/to/am-hook [--tag v0.5.9]" >&2; exit 2; fi
PATCH=$REPO/patches/am-hook-joc-$TAG.patch
[ -f "$PATCH" ] || { echo "missing $PATCH (run scripts/make-patch.sh)" >&2; exit 1; }
# A Windows checkout may have converted the patch to CRLF; git apply wants LF.
CLEAN_PATCH=$(mktemp)
trap 'rm -f "$CLEAN_PATCH"' EXIT
tr -d '' < "$PATCH" > "$CLEAN_PATCH"
PATCH=$CLEAN_PATCH

cd "$DIR"
if [ ! -d .git ]; then echo "error: $DIR is not a git checkout (the patch is applied with git apply)" >&2; exit 1; fi
DESCRIBED=$(git describe --tags --always 2>/dev/null || true)
if [ "$DESCRIBED" != "$TAG" ]; then
  echo "warning: $DIR is at $DESCRIBED, expected the locked release $TAG" >&2
fi

# The JOC engine in player.js is the marker; a tree that has it is patched (the
# HRTF/asset files it also carries are build products and differ per build).
if grep -q "class JocEngine" src/ui/player.js 2>/dev/null; then
  echo "patch already applied to $DIR"
  exit 0
fi
# Patches may carry either a/src/... or a/am-hook/src/... prefixes.
STRIP=1
if git apply -p1 --reverse --check "$PATCH" >/dev/null 2>&1; then
  echo "patch already applied to $DIR"
  exit 0
fi
if git apply -p2 --reverse --check "$PATCH" >/dev/null 2>&1; then
  echo "patch already applied to $DIR"
  exit 0
fi
if ! git apply -p1 --check "$PATCH" >/dev/null 2>&1; then
  if git apply -p2 --check "$PATCH" >/dev/null 2>&1; then
    STRIP=2
  fi
fi
git apply -p"$STRIP" --check "$PATCH"
if [ "$CHECK_ONLY" = 1 ]; then
  echo "patch applies cleanly to $DIR"
  exit 0
fi
git apply -p"$STRIP" "$PATCH"
echo "applied $(basename "$PATCH") to $DIR"
