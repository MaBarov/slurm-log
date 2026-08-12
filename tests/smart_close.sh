#!/bin/sh
# Offline regression for one-log-pane close detection. No scheduler commands.
set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=${SLURM_LOG_TEST_BINARY:-$project_dir/target/release/slurm-log}
session=slurm-logs-smart-close-$$
cleanup() { tmux kill-session -t "$session" >/dev/null 2>&1 || true; }
trap cleanup EXIT HUP INT TERM

first=$(tmux new-session -d -P -F '#{pane_id}' -s "$session" sleep 60)
# Match the single-process batched labeling transaction used by the Rust code.
tmux set-option -p -t "$first" @slurm_log_cluster sprint \
    ';' set-option -p -t "$first" @slurm_log_job_id 1 \
    ';' select-pane -t "$first" -T sprint:1
test "$(tmux show-options -p -v -t "$first" @slurm_log_cluster)" = sprint
test "$(tmux show-options -p -v -t "$first" @slurm_log_job_id)" = 1
test "$(tmux display-message -p -t "$first" '#{pane_title}')" = sprint:1
"$binary" single-pane "$session"

detail=$(tmux split-window -d -P -F '#{pane_id}' -t "$first" sleep 60)
tmux set-option -p -t "$detail" @slurm_log_detail_parent "$first"
"$binary" single-pane "$session"

second=$(tmux split-window -d -P -F '#{pane_id}' -t "$first" sleep 60)
tmux set-option -p -t "$second" @slurm_log_cluster cispa
tmux set-option -p -t "$second" @slurm_log_job_id 2
if "$binary" single-pane "$session"; then
    printf 'two log panes were incorrectly treated as a single panel\n' >&2
    exit 1
fi

# tmux must accept the same queued popup/refresh sequence used by Ctrl-b j.
tmux bind-key -T prefix C-g display-popup -E true '\;' refresh-client
tmux list-keys -T prefix C-g | grep -q 'refresh-client'
printf 'smart_close: ok (details ignored; multiple logs protected)\n'
