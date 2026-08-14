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
# Process coverage is fully hermetic and must resolve the fake Slurm/SSH/tmux
# executables supplied by each test. The release workflow builds and packages
# the production binary separately, without this test-only cfg.
test_public_key=7777777777777777777777777777777777777777777777777777777777777777
# cargo-llvm-cov emits quoted, tool-owned assignments for the active rustc.
eval "$(cargo llvm-cov show-env --sh)"

printf '%s\n' '==> instrumented unit tests'
cargo test --locked --release --manifest-path "$project_dir/Cargo.toml" --quiet
printf '%s\n' '==> instrumented offline performance tests'
cargo test --locked --release --manifest-path "$project_dir/Cargo.toml" --quiet -- --ignored
printf '%s\n' '==> instrumented release binary'
SLURM_LOG_TEST_BUILD=1 SLURM_LOG_TEST_RELEASE_PUBLIC_KEY=$test_public_key \
    cargo build --locked --release --manifest-path "$project_dir/Cargo.toml" --quiet
binary=$CARGO_TARGET_DIR/release/slurm-log

for name in package_smoke offline_hostile follower_paths pane_close interactive_pane details_pane details_direct \
    focus_toast cli_surface picker_controls daemon_integration \
    workspace_controls reconcile_paths bank_actions bank_ui cluster_tabs degraded_clusters \
    smart_close setup_wizard mcp_server mcp_tools mcp_setup mutation_bindings mcp_owner_isolation; do
    printf '==> integration: %s\n' "$name"
    if [ "$name" = package_smoke ]; then
        # This test deliberately switches between test-key and production-key
        # packaging. Do not leak the test-build cfg into its production probe.
        env -u SLURM_LOG_TEST_BUILD -u SLURM_LOG_TEST_RELEASE_PUBLIC_KEY \
            SLURM_LOG_TEST_BINARY=$binary "$project_dir/tests/$name.sh"
    elif [ "$name" = mcp_setup ]; then
        SLURM_LOG_TEST_BUILD=1 SLURM_LOG_TEST_RELEASE_PUBLIC_KEY=$test_public_key \
            SLURM_LOG_TEST_BINARY=$binary sh "$project_dir/tests/$name.sh"
    else
        SLURM_LOG_TEST_BUILD=1 SLURM_LOG_TEST_RELEASE_PUBLIC_KEY=$test_public_key \
            SLURM_LOG_TEST_BINARY=$binary "$project_dir/tests/$name.sh"
    fi
done

minimum=${SLURM_LOG_COVERAGE_MINIMUM:-95}
printf '==> merged full-stack coverage (minimum %s%% lines)\n' "$minimum"
if test -n "${SLURM_LOG_COVERAGE_LCOV:-}"; then
    cargo llvm-cov report --release --lcov --output-path "$SLURM_LOG_COVERAGE_LCOV"
fi
cargo llvm-cov report --release --summary-only --fail-under-lines "$minimum"
