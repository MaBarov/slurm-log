#!/bin/sh
# Complete offline validation suite.
#
# Runs formatting checks, debug correctness/security/concurrency tests, ignored
# release performance budgets, shell syntax checks, and an isolated package +
# daemon lifecycle smoke test. It never contacts SLURM or SSH services.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
manifest=$project_dir/Cargo.toml

run_check() {
    label=$1
    shift
    printf '==> %s\n' "$label"
    if "$@"; then
        return 0
    fi
    if [ "${GITHUB_ACTIONS:-}" = true ]; then
        printf '::error title=Offline suite failed::%s\n' "$label"
    fi
    exit 1
}

run_check format cargo fmt --manifest-path "$manifest" -- --check
run_check clippy cargo clippy --locked --manifest-path "$manifest" --all-targets -- -D warnings
run_check correctness cargo test --locked --manifest-path "$manifest"
run_check performance cargo test --locked --release --manifest-path "$manifest" -- --ignored
# Integration tests execute target/release/slurm-log, which is distinct from
# Cargo's release test harness and must never be allowed to remain stale.
run_check release-build cargo build --locked --release --manifest-path "$manifest"
run_check install-syntax sh -n "$project_dir/install.sh"
run_check update-syntax sh -n "$project_dir/update.sh"
run_check uninstall-syntax sh -n "$project_dir/uninstall.sh"
run_check package-syntax sh -n "$project_dir/package.sh"
run_check release-workflow test -f "$project_dir/.github/workflows/release.yml"
run_check rust-workflow test -f "$project_dir/.github/workflows/rust.yml"
run_check security-audit-syntax sh -n "$project_dir/security-audit.sh"
run_check coverage-syntax sh -n "$project_dir/coverage.sh"
for test_script in package_smoke offline_hostile follower_paths pane_close interactive_pane \
    details_pane details_direct focus_toast cli_surface picker_controls daemon_integration \
    workspace_controls reconcile_paths feature_coverage bank_actions bank_ui cluster_tabs \
    degraded_clusters smart_close setup_wizard mcp_server source_layout; do
    run_check "$test_script-syntax" sh -n "$project_dir/tests/$test_script.sh"
done
run_check source_layout "$project_dir/tests/source_layout.sh"
run_check package_smoke "$project_dir/tests/package_smoke.sh"
run_check offline_hostile "$project_dir/tests/offline_hostile.sh"
run_check follower_paths "$project_dir/tests/follower_paths.sh"
run_check pane_close "$project_dir/tests/pane_close.sh"
run_check interactive_pane "$project_dir/tests/interactive_pane.sh"
run_check details_pane "$project_dir/tests/details_pane.sh"
run_check details_direct "$project_dir/tests/details_direct.sh"
run_check focus_toast "$project_dir/tests/focus_toast.sh"
run_check cli_surface "$project_dir/tests/cli_surface.sh"
run_check picker_controls "$project_dir/tests/picker_controls.sh"
run_check daemon_integration "$project_dir/tests/daemon_integration.sh"
run_check workspace_controls "$project_dir/tests/workspace_controls.sh"
run_check reconcile_paths "$project_dir/tests/reconcile_paths.sh"
run_check feature_coverage "$project_dir/tests/feature_coverage.sh"
run_check bank_actions "$project_dir/tests/bank_actions.sh"
run_check bank_ui "$project_dir/tests/bank_ui.sh"
run_check cluster_tabs "$project_dir/tests/cluster_tabs.sh"
run_check degraded_clusters "$project_dir/tests/degraded_clusters.sh"
run_check smart_close "$project_dir/tests/smart_close.sh"
run_check setup_wizard "$project_dir/tests/setup_wizard.sh"
run_check mcp_server "$project_dir/tests/mcp_server.sh"
printf 'all slurm-log tests passed\n'
