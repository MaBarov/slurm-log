#!/bin/sh
# Actual source coverage from unit tests plus the offline process-level suite.
# Builds only in a temporary directory and never contacts Slurm or SSH.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
command -v cargo-llvm-cov >/dev/null 2>&1 || {
    printf 'coverage: install cargo-llvm-cov first: cargo install cargo-llvm-cov --locked\n' >&2
    exit 1
}
coverage_root=$(mktemp -d)
case "$coverage_root" in /tmp/*) ;; *) exit 1 ;; esac
trap 'rm -rf "$coverage_root"' EXIT HUP INT TERM

export CARGO_TARGET_DIR=$coverage_root/target
# cargo-llvm-cov emits quoted, tool-owned assignments for the active rustc.
eval "$(cargo llvm-cov show-env --sh)"

printf '%s\n' '==> instrumented unit tests'
cargo test --locked --release --manifest-path "$project_dir/Cargo.toml" --quiet
printf '%s\n' '==> instrumented offline performance tests'
cargo test --locked --release --manifest-path "$project_dir/Cargo.toml" --quiet -- --ignored
printf '%s\n' '==> instrumented release binary'
cargo build --locked --release --manifest-path "$project_dir/Cargo.toml" --quiet
binary=$CARGO_TARGET_DIR/release/slurm-log

for name in package_smoke offline_hostile follower_paths pane_close interactive_pane details_pane details_direct \
    focus_toast cli_surface picker_controls daemon_integration \
    workspace_controls reconcile_paths bank_actions bank_ui cluster_tabs degraded_clusters \
    smart_close setup_wizard; do
    printf '==> integration: %s\n' "$name"
    SLURM_LOG_TEST_BINARY=$binary "$project_dir/tests/$name.sh"
done

minimum=${SLURM_LOG_COVERAGE_MINIMUM:-95}
printf '==> merged full-stack coverage (minimum %s%% lines)\n' "$minimum"
if test -n "${SLURM_LOG_COVERAGE_LCOV:-}"; then
    cargo llvm-cov report --release --lcov --output-path "$SLURM_LOG_COVERAGE_LCOV"
fi
cargo llvm-cov report --release --summary-only --fail-under-lines "$minimum"
