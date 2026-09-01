#!/bin/sh
# Regenerate the desktop application's TypeScript from the Rust types it talks
# to. Run this after changing anything on the wire; CI fails if the checked-in
# files disagree with what this produces.
#
# The types are generated rather than written because there is no honest way to
# keep two copies of a message in step by hand -- the drift shows up as a
# runtime shape mismatch in a webview, which is the worst place to find it.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
out="$root/apps/desktop/src/gen"

mkdir -p "$out"
# Stale files are not overwritten by a type that no longer exists, so the
# directory is emptied first: a binding left behind after its type was deleted
# is one an import can still find.
rm -f "$out"/*.ts

TS_RS_EXPORT_DIR="$out" cargo test --features ts \
    -p sbx-core -p sbx-proto export_bindings -- --quiet

echo "wrote $(find "$out" -name '*.ts' | wc -l) files to apps/desktop/src/gen"
