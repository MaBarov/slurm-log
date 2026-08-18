#!/bin/sh
# Online dependency and source-security audit. The release workflow pins
# cargo-audit 0.22.2, cargo-deny 0.20.2, and cargo-geiger 0.13.0 with --locked.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
manifest=$project_dir/Cargo.toml

cargo audit --file "$project_dir/Cargo.lock" --deny warnings
cargo deny --manifest-path "$manifest" check advisories bans licenses sources
# The compiler-enforced `#![forbid(unsafe_code)]` is authoritative for this
# crate. Keep Geiger diagnostics visible and fail closed if it reports a
# dependency matching/parsing failure rather than presenting partial coverage
# as a clean result.
geiger_log=$(mktemp)
trap 'rm -f "$geiger_log"' EXIT HUP INT TERM
cargo geiger --manifest-path "$manifest" --locked --forbid-only --quiet \
    >"$geiger_log" 2>&1 || {
        cat "$geiger_log" >&2
        exit 1
    }
cat "$geiger_log"
if grep -Eq 'Failed to (match|parse)' "$geiger_log"; then
    printf 'cargo geiger did not complete dependency analysis.\n' >&2
    exit 1
fi
printf 'all slurm-log dependency security checks passed\n'
