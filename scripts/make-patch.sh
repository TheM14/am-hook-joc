#!/usr/bin/env bash
# Generates the patch against the locked upstream release.
#
# The patch is a plain unified diff produced from two directory trees (no git
# metadata is written to the upstream checkout):
#
#   a/  the pristine upstream source of the locked tag
#   b/  this workspace's working copy, minus build outputs and minus the HRTF
#
# The compiled `.jochrtf` fields are binaries: they live in hrtf/ and are
# installed by scripts/build.sh, so the patch stays text-only.
set -eu
cd "$(dirname "$0")/.."
REPO=$(pwd)
UPSTREAM=${UPSTREAM:-../am-hook}
BASE=${BASE:-$UPSTREAM}
TAG=${TAG:-$(git -C "$BASE" describe --tags --abbrev=0 2>/dev/null || true)}
if [ -z "$TAG" ]; then echo "error: set TAG to the upstream release to diff against (BASE must be a git checkout with that tag)" >&2; exit 2; fi
WORK=${WORK:-$(mktemp -d)}
PATCH=${PATCH:-patches/am-hook-joc.patch}

mkdir -p "$WORK/a" "$WORK/b"
echo "upstream: $UPSTREAM @ $TAG"
git -C "$BASE" archive "$TAG" | tar -x -C "$WORK/a"

# Copy the working tree without VCS data, build output, or the HRTF assets.
tar -C "$UPSTREAM" -cf - \
  --exclude=.git \
  --exclude=target \
  --exclude=node_modules \
  --exclude=src/ui/joc.wasm \
  --exclude='src/ui/joc-hrtf-*.jochrtf' \n  --exclude='*.rej' \n  --exclude='*.orig' \
  . | tar -x -C "$WORK/b"

# The workspace itself carries no incidental files; keep the diff reviewable.
rm -rf "$WORK/b/.idea" "$WORK/b/dist" 2>/dev/null || true

# A Windows checkout may have CRLF (git archive included). Normalise both sides
# so the patch is identical on every platform and has no spurious differences.
python - "$WORK/a" "$WORK/b" <<'PYNORM'
import pathlib, sys
CRLF = b"\x0d\x0a"
LF = b"\x0a"
for root in sys.argv[1:]:
    for path in pathlib.Path(root).rglob("*"):
        if path.is_file():
            path.write_bytes(path.read_bytes().replace(CRLF, LF))
PYNORM

mkdir -p "$(dirname "$REPO/$PATCH")"
( cd "$WORK" && diff -ruN --exclude=.DS_Store a b ) > "$REPO/$PATCH" || true
# diff exits 1 when the trees differ, which is the expected case.
if [ ! -s "$REPO/$PATCH" ]; then
  echo "error: empty patch" >&2
  exit 1
fi
if grep -q "^--- a/am-hook/" "$REPO/$PATCH"; then
  echo "error: patch paths must be relative to the am-hook checkout (a/src/...)" >&2
  exit 1
fi
echo "wrote $PATCH ($(wc -l < "$REPO/$PATCH") lines)"
echo "workdir kept at $WORK"
