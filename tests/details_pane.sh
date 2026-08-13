#!/bin/sh
# Offline tmux regression for the per-log Ctrl-b i auxiliary pane. No SSH or
# scheduler command is allowed to resolve outside the temporary fake PATH.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=${SLURM_LOG_TEST_BINARY:-$project_dir/target/release/slurm-log}
test_root=$(mktemp -d)
session=slurm-log-details-test-$$
case "$test_root" in /tmp/*) ;; *) exit 1 ;; esac
cleanup() {
    tmux kill-session -t "$session" >/dev/null 2>&1 || true
    "$binary" daemon stop --state-path "$test_root/state.json" >/dev/null 2>&1 || true
    rm -rf "$test_root"
}
trap cleanup EXIT HUP INT TERM

fake_bin=$test_root/fake-bin
call_log=$test_root/calls
mkdir -p "$fake_bin"
cat >"$fake_bin/ssh" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"$DETAILS_PANE_CALL_LOG"
case "$*" in
    *'squeue -h'*' -j 42 '*)
        printf '42|offline|PENDING|offline-job|00:00|Resources|gpu|Unknown|1|sbatch\n'
        ;;
    *'squeue -h'*' -j 3209343_2 '*)
        printf '3209343_2|offline|RUNNING|array-job|00:01:30|node-1|gpu|2026-08-12T10:00:00|2|sbatch\n'
        ;;
    *'squeue -h'*)
        printf '42|PENDING|offline-job|00:00|Resources|gpu|Unknown|1|offline\n3209343_2|RUNNING|array-job|00:01:30|node-1|gpu|2026-08-12T10:00:00|2|offline\n'
        ;;
    *'show job -o 3209343_2'*)
        printf 'JobId=3209343_2 UserId=offline(1000) JobName=array-job JobState=RUNNING Reason=None RunTime=00:01:30 TimeLimit=01:00:00 NumNodes=1 NumCPUs=4 Partition=gpu NodeList=node-1 Account=lab QOS=normal ReqTRES=cpu=4,mem=8G,gres/gpu=2 AllocTRES=cpu=4,mem=8G,gres/gpu=2 ExitCode=0:0\n'
        ;;
    *'sstat '*'-a -j 3209343_2'*)
        printf '3209343_2.batch|4|cpu=4,mem=8G,gres/gpu=2|00:01:00|2G|1G|gres/gpuutil=70|gres/gpumem=3G\n'
        ;;
    *sacct*) exit 91 ;;
    *) exit 23 ;;
esac
EOF
chmod 755 "$fake_bin/ssh"
export PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin"
export DETAILS_PANE_CALL_LOG=$call_log
config=$test_root/config.json
cat >"$config" <<EOF
{"clusters":[{"name":"cispa","transport":"ssh","user":"offline","sshHost":"offline.invalid","workingDirectory":"$test_root","accounting":true}],"statePath":"$test_root/state.json"}
EOF
export SLURM_LOG_CONFIG=$config

tmux new-session -d -s "$session" sh -c 'while :; do sleep 60; done'
tmux set-option -w -t "$session" remain-on-exit on
tmux set-environment -t "$session" PATH "$fake_bin:/usr/local/bin:/usr/bin:/bin"
tmux set-environment -t "$session" DETAILS_PANE_CALL_LOG "$call_log"
tmux set-environment -t "$session" SLURM_LOG_CONFIG "$config"
pane=$(tmux display-message -p -t "$session" '#{pane_id}')
tmux set-option -p -t "$pane" @slurm_log_cluster cispa
tmux set-option -p -t "$pane" @slurm_log_job_id 42
tmux set-option -p -t "$pane" @slurm_log_job_name offline-job

common="--local-user offline --remote-user offline --ssh-host offline.invalid --state-path $test_root/state.json"
# Plain panes are rejected, and a terminal too small to split reports tmux's
# bounded reason without damaging the owning log pane.
plain=$(tmux split-window -d -P -F '#{pane_id}' -t "$pane" 'sleep 120')
# shellcheck disable=SC2086
if "$binary" $common toggle-details "$plain" >"$test_root/plain.out" 2>"$test_root/plain.err"; then
    exit 1
fi
grep -F 'focused pane is not a slurm-log job' "$test_root/plain.err" >/dev/null
tmux kill-pane -t "$plain"
tmux resize-window -t "$session" -y 2
# shellcheck disable=SC2086
if "$binary" $common toggle-details "$pane" >"$test_root/small.out" 2>"$test_root/small.err"; then
    exit 1
fi
grep -F 'could not open the details pane' "$test_root/small.err" >/dev/null
tmux resize-window -t "$session" -y 20

# Validate the non-interactive information report and warm the private daemon
# with the exact same fully fake scheduler environment used by the PTY pane.
text_report=$test_root/details.txt
# shellcheck disable=SC2086
env PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" \
    DETAILS_PANE_CALL_LOG="$call_log" \
    "$binary" $common details 42 --cluster cispa >"$text_report"
grep -F 'Job: cispa:42 offline-job' "$text_report" >/dev/null
grep -F 'State: PENDING Resources' "$text_report" >/dev/null
grep -F 'Sample:' "$text_report" >/dev/null

# A running array task must use live data and never depend on lagging sacct.
array_report=$test_root/array-details.txt
# shellcheck disable=SC2086
env PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" \
    DETAILS_PANE_CALL_LOG="$call_log" \
    "$binary" $common details 3209343_2 --cluster cispa >"$array_report"
grep -F 'Job: cispa:3209343_2 array-job' "$array_report" >/dev/null
grep -F 'State: RUNNING' "$array_report" >/dev/null
grep -F '1 nodes, 4 CPUs, 2 GPUs' "$array_report" >/dev/null
grep -F '(sstat)' "$array_report" >/dev/null
! grep -F sacct "$call_log" >/dev/null

wait_for_text() {
    target=$1
    wanted=$2
    attempt=0
    while :; do
        screen=$(tmux capture-pane -p -t "$target" 2>/dev/null || true)
        printf '%s\n' "$screen" | grep -F "$wanted" >/dev/null && return 0
        attempt=$((attempt + 1))
        test "$attempt" -lt 500 || {
            printf 'Details pane did not show %s:\n%s\n' "$wanted" "$screen" >&2
            test ! -f "$call_log" || { printf 'Scheduler calls:\n' >&2; cat "$call_log" >&2; }
            tmux list-panes -a -F '#{pane_id}|#{pane_dead}|#{pane_dead_status}|#{pane_start_command}' >&2 || true
            exit 1
        }
        sleep 0.01
    done
}
wait_for_panes() {
    wanted=$1
    attempt=0
    while test "$(tmux list-panes -t "$session" | wc -l)" -ne "$wanted"; do
        attempt=$((attempt + 1))
        test "$attempt" -lt 500 || exit 1
        sleep 0.01
    done
}
open_details() {
    # shellcheck disable=SC2086
    "$binary" $common toggle-details "$pane"
    tmux list-panes -t "$session" -F '#{pane_id}|#{@slurm_log_detail_parent}' |
        awk -F '|' -v parent="$pane" '$2 == parent { print $1 }'
}

# shellcheck disable=SC2086
details=$(open_details)
test -n "$details"
test "$(tmux list-panes -t "$session" | wc -l)" -eq 2
# Auxiliary panes inherit identity metadata so the persistent green status bar
# keeps naming the same job while details has keyboard focus.
test "$(tmux show-options -pv -t "$details" @slurm_log_cluster)" = cispa
test "$(tmux show-options -pv -t "$details" @slurm_log_job_id)" = 42
test "$(tmux show-options -pv -t "$details" @slurm_log_job_name)" = offline-job
# The pane advertises direct r/Space/q/Enter controls, so opening it must also
# give it keyboard focus. The old detached split sent those keys to the log.
test "$(tmux display-message -p -t "$session" '#{pane_id}')" = "$details"
wait_for_text "$details" 'DETAILS  cispa:42'
wait_for_text "$details" 'PENDING'
wait_for_text "$details" 'squeue'

# Pause/resume must repaint immediately, refresh must keep the pane alive, and
# Enter must close it cleanly.
tmux send-keys -t "$session" Space
wait_for_text "$details" 'paused'
tmux send-keys -t "$session" Space
wait_for_text "$details" 'auto 30s'
tmux send-keys -t "$session" r
wait_for_text "$details" 'refresh queued (10s rate limit)'
test "$(tmux list-panes -t "$session" | wc -l)" -eq 2
tmux send-keys -t "$details" Enter
wait_for_panes 1

# All documented direct close keys work in a real PTY.
details=$(open_details)
wait_for_text "$details" 'DETAILS  cispa:42'
tmux send-keys -t "$details" q
wait_for_panes 1
details=$(open_details)
wait_for_text "$details" 'DETAILS  cispa:42'
tmux send-keys -t "$details" Escape
wait_for_panes 1

# Toggling from the owning log closes the paired details pane.
details=$(open_details)
test -n "$details"
# shellcheck disable=SC2086
"$binary" $common toggle-details "$pane"
test "$(tmux list-panes -t "$session" | wc -l)" -eq 1

# Reopen and toggle from inside the details pane itself.
details=$(open_details)
# shellcheck disable=SC2086
"$binary" $common toggle-details "$details"
test "$(tmux list-panes -t "$session" | wc -l)" -eq 1
test ! -e "$call_log" || ! grep -qv 'squeue -h' "$call_log"

printf 'details_pane: ok (render, pause/resume, refresh, all close paths, pairing; fully offline)\n'
