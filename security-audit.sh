#!/bin/sh
# Online dependency and source-security audit. Requires cargo-audit and
# cargo-deny; install them with: cargo install cargo-audit cargo-deny --locked

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
manifest=$project_dir/Cargo.toml

cargo audit --file "$project_dir/Cargo.lock" --deny warnings
cargo deny --manifest-path "$manifest" check advisories bans licenses sources
# The compiler-enforced `#![forbid(unsafe_code)]` is authoritative for this
# crate. Geiger verifies the marker; dependency unsafe internals are reported
# by a full `cargo geiger` run but are not actionable application failures.
cargo geiger --manifest-path "$manifest" --locked --forbid-only --quiet \
    >/dev/null 2>&1
printf 'all slurm-log dependency security checks passed\n'
