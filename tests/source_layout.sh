#!/bin/sh
# Keeps hand-maintained Rust and shell files reviewable. Generated package and
# build output under dist/ and target/ are deliberately outside this check.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
limit=450
failed=0

files=$(find "$project_dir/src" "$project_dir/tests" -type f \
    \( -name '*.rs' -o -name '*.sh' \) -print)
files="$files
$(find "$project_dir" -maxdepth 1 -type f -name '*.sh' -print)"

for file in $files; do
    lines=$(wc -l <"$file")
    if [ "$lines" -gt "$limit" ]; then
        printf '%s has %s lines (maximum %s)\n' "$file" "$lines" "$limit" >&2
        failed=1
    fi
done

test "$failed" -eq 0
printf 'source_layout: ok (all maintained Rust/shell files <= %s lines)\n' "$limit"
