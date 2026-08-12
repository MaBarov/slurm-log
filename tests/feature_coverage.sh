#!/bin/sh
# Regression guard for the integration matrix. It prevents a documented public
# command/option or essential control from silently losing an owning test.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=${SLURM_LOG_TEST_BINARY:-$project_dir/target/release/slurm-log}
manifest=$project_dir/tests/feature_manifest.tsv
test -r "$manifest"

fail() { printf 'feature_coverage: %s\n' "$*" >&2; exit 1; }
has_feature() {
    awk -F '\t' -v feature="$1" 'NR > 1 && $1 == feature { found=1 } END { exit !found }' "$manifest"
}
require_feature() { has_feature "$1" || fail "missing manifest entry: $1"; }

# The manifest itself is normalized, unique, and points only at executable
# offline integration scripts in this package.
awk -F '\t' '
    NR == 1 { if ($1 != "feature" || $2 != "test") exit 1; next }
    NF != 2 || $1 !~ /^[a-z0-9_.A-Z-]+$/ || $2 !~ /^[a-z0-9_.-]+[.]sh$/ { exit 1 }
' "$manifest" || fail 'malformed feature manifest'
duplicates=$(tail -n +2 "$manifest" | cut -f1 | sort | uniq -d)
test -z "$duplicates" || fail "duplicate features: $duplicates"
for owner in $(tail -n +2 "$manifest" | cut -f2 | sort -u); do
    test -x "$project_dir/tests/$owner" || fail "missing executable owner: $owner"
done

help=$($binary --help)
views='all running failed blocked archive last watch fzf'
commands='setup details bank submit cancel read unread json sessions attach close daemon update uninstall'
for view in $views; do
    require_feature "cli.view.$view"
    printf '%s\n' "$help" | grep -E "^  $view([[:space:]]|$)" >/dev/null || fail "view absent from help: $view"
done
for command in $commands; do
    require_feature "cli.command.$command"
    printf '%s\n' "$help" | grep -E "(^  $command([[:space:]]|$)|^  slurm-log $command([[:space:]]|$))" >/dev/null ||
        fail "command absent from help: $command"
done

for option in lines cluster refresh bank_dir follow fzf show_log_warnings local_user remote_user ssh_host state_path binary purge; do
    require_feature "cli.option.$option"
done
for spelling in '--lines' '--cluster' '--refresh' '--bank-dir' '--follow' '--fzf' \
    '--show-log-warnings' '--local-user' '--remote-user' '--ssh-host' '--state-path' \
    '--binary' '--purge'; do
    printf '%s\n' "$help" | grep -F -- "$spelling" >/dev/null || fail "option absent from help: $spelling"
done

# Essential interactive controls remain represented even though their detailed
# reference is rendered inside the picker rather than by --help.
for feature in \
    picker.navigation.arrows_jk picker.navigation.page_home_end_g \
    picker.group.expand_collapse picker.selection.space_group \
    picker.selection.all_clear picker.selection.enter_exact picker.dismiss \
    picker.cancel_jobs picker.open_bank picker.cluster_tabs \
    picker.history_horizon_cycle picker.archive_toggle picker.blocked_toggle \
    picker.refresh picker.search_apply_cancel_clear picker.notices_toggle \
    picker.warnings_toggle picker.details picker.auto_add_toggle \
    picker.reference_page picker.quit workspace.popup_j_a workspace.details_i \
    workspace.auto_add_A workspace.close_pane_x workspace.zoom_z \
    workspace.smart_close_q workspace.mouse_selection_copy; do
    require_feature "$feature"
done

for feature in details.ambiguous_cluster details.missing_cluster_hint cluster.partial_failure; do
    require_feature "$feature"
done

count=$(awk 'END { print NR - 1 }' "$manifest")
test "$count" -ge 100 || fail "integration matrix unexpectedly shrank to $count entries"
printf 'feature_coverage: ok (%s command/feature contracts mapped to offline integration tests)\n' "$count"
