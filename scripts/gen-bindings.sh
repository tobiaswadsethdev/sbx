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

files=$(find "$out" -name '*.ts' | wc -l)

# **Every exported type lands in this one flat directory, so two Rust types with
# one name silently become one file.** It has happened twice: `files::Entry` and
# `git::Entry`, which surfaced as a type error because their shapes differed,
# and `integrations::View` against `policy::View`, which did not -- the generated
# `Reply` carried `{ "reply": "integrations" } & View` pointing at the policy
# view, and a webview would have found that out at runtime.
#
# So the count is checked. One `ts(export)` attribute is one file; fewer files
# than attributes means two types answered to one name, and `ts(rename = "...")`
# on one of them is the fix. Counted from attribute lines only, so a comment
# mentioning the attribute -- there is one -- does not inflate the total.
exported=$(grep -rhE '^[[:space:]]*#\[.*ts\(export' "$root/crates" --include='*.rs' | wc -l)
if [ "$files" -ne "$exported" ]; then
    echo "error: $exported types exported but $files files written." >&2
    echo "Two exported types share a name; give one \`ts(rename = \"...\")\`." >&2
    exit 1
fi

echo "wrote $files files to apps/desktop/src/gen"
