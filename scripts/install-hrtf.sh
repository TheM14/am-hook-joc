#!/usr/bin/env bash
# Installs the compiled HRTF fields (hrtf/*.jochrtf) into a patched checkout.
#
#   scripts/install-hrtf.sh /path/to/am-hook
#
# The patch registers one asset per field, so a plain `cargo build` needs these
# files in src/ui/; scripts/build.sh calls this script before building.
set -eu
cd "$(dirname "$0")/.."
REPO=$(pwd)
DIR=${1:-}
if [ -z "$DIR" ]; then echo "usage: scripts/install-hrtf.sh /path/to/am-hook" >&2; exit 2; fi
[ -d "$DIR/src/ui" ] || { echo "error: $DIR does not look like an am-hook checkout" >&2; exit 1; }

HRTF_DIR="$REPO/hrtf"
[ -d "$HRTF_DIR" ] || { echo "error: $HRTF_DIR is missing" >&2; exit 1; }
count=0
for field in "$HRTF_DIR"/*.jochrtf; do
  [ -f "$field" ] || continue
  name=$(basename "$field" .jochrtf)
  cp "$field" "$DIR/src/ui/joc-hrtf-$name.jochrtf"
  count=$((count + 1))
done
if [ "$count" -eq 0 ]; then
  echo "error: no .jochrtf fields in $HRTF_DIR" >&2
  exit 1
fi
echo "installed $count HRTF fields"
