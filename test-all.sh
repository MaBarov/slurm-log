#!/bin/sh
# Complete offline validation suite.
#
# Runs formatting checks, debug correctness/security/concurrency tests, ignored
# release performance budgets, shell syntax checks, and an isolated package +
# daemon lifecycle smoke test. It never contacts SLURM or SSH services.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
manifest=$project_dir/Cargo.toml

cargo fmt --manifest-path "$manifest" -- --check
cargo clippy --locked --manifest-path "$manifest" --all-targets -- -D warnings
cargo test --locked --manifest-path "$manifest"
cargo test --locked --release --manifest-path "$manifest" -- --ignored
# Integration tests execute target/release/slurm-log, which is distinct from
# Cargo's release test harness and must never be allowed to remain stale.
cargo build --locked --release --manifest-path "$manifest"
sh -n "$project_dir/install.sh"
sh -n "$project_dir/update.sh"
sh -n "$project_dir/uninstall.sh"
sh -n "$project_dir/package.sh"
sh -n "$project_dir/security-audit.sh"
sh -n "$project_dir/tests/package_smoke.sh"
sh -n "$project_dir/tests/offline_hostile.sh"
sh -n "$project_dir/tests/pane_close.sh"
sh -n "$project_dir/tests/interactive_pane.sh"
sh -n "$project_dir/tests/details_pane.sh"
sh -n "$project_dir/tests/focus_toast.sh"
sh -n "$project_dir/tests/cli_surface.sh"
sh -n "$project_dir/tests/picker_controls.sh"
sh -n "$project_dir/tests/daemon_integration.sh"
sh -n "$project_dir/tests/workspace_controls.sh"
sh -n "$project_dir/tests/feature_coverage.sh"
sh -n "$project_dir/tests/bank_actions.sh"
sh -n "$project_dir/tests/bank_ui.sh"
sh -n "$project_dir/tests/cluster_tabs.sh"
sh -n "$project_dir/tests/smart_close.sh"
sh -n "$project_dir/tests/setup_wizard.sh"
sh -n "$project_dir/tests/source_layout.sh"
"$project_dir/tests/source_layout.sh"
"$project_dir/tests/package_smoke.sh"
"$project_dir/tests/offline_hostile.sh"
"$project_dir/tests/pane_close.sh"
"$project_dir/tests/interactive_pane.sh"
"$project_dir/tests/details_pane.sh"
"$project_dir/tests/focus_toast.sh"
"$project_dir/tests/cli_surface.sh"
"$project_dir/tests/picker_controls.sh"
"$project_dir/tests/daemon_integration.sh"
"$project_dir/tests/workspace_controls.sh"
"$project_dir/tests/feature_coverage.sh"
"$project_dir/tests/bank_actions.sh"
"$project_dir/tests/bank_ui.sh"
"$project_dir/tests/cluster_tabs.sh"
"$project_dir/tests/smart_close.sh"
"$project_dir/tests/setup_wizard.sh"
printf 'all slurm-log tests passed\n'
