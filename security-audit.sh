#!/bin/sh
# Online dependency and source-security audit. The release workflow pins
# cargo-audit 0.22.2, cargo-deny 0.20.2, and cargo-geiger 0.13.0 with --locked.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
manifest=$project_dir/Cargo.toml

cargo audit --file "$project_dir/Cargo.lock" --deny warnings
cargo deny --manifest-path "$manifest" check advisories bans licenses sources
# The compiler-enforced `#![forbid(unsafe_code)]` is authoritative for this
# crate. Scope Geiger to our root package: `--forbid-only` validates entry
# points, while third-party unsafe-code policy is covered by the advisory and
# source checks above. Geiger 0.13 can emit parser warnings for Syn 3
# third-party sources, so require its positive root-package verdict instead of
# treating unrelated parser warnings as a false clean/fail result.
geiger_log=$(mktemp)
trap 'rm -f "$geiger_log"' EXIT HUP INT TERM
cargo geiger --manifest-path "$manifest" --package slurm-log --locked --forbid-only --quiet \
    >"$geiger_log" 2>&1 || {
        cat "$geiger_log" >&2
        exit 1
    }
cat "$geiger_log"
if ! grep -Eq '^:\) slurm-log [0-9]+\.[0-9]+\.[0-9]+$' "$geiger_log"; then
    printf 'cargo geiger did not verify slurm-log has forbid(unsafe_code).\n' >&2
    exit 1
fi
printf 'all slurm-log dependency security checks passed\n'
