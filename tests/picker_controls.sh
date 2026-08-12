#!/bin/sh
# Fully offline PTY regression for every documented built-in picker control.
# A private tmux server and fake scheduler keep it isolated from user sessions.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=${SLURM_LOG_TEST_BINARY:-$project_dir/target/release/slurm-log}
test_root=$(mktemp -d)
case "$test_root" in /tmp/*) ;; *) exit 1 ;; esac
tmux_root=$test_root/tmux
fake_bin=$test_root/bin
state=$test_root/state/state.json
config=$test_root/config.json
calls=$test_root/calls
main_session=picker-controls-$$
open_session=picker-open-$$
mkdir -p "$tmux_root" "$fake_bin" "$test_root/state" "$test_root/bank"
chmod 700 "$tmux_root"

tmux_test() {
    env TMUX_TMPDIR="$tmux_root" tmux "$@"
}
cleanup() {
    for session in "$main_session" "$open_session"; do
        tmux_test kill-session -t "$session" >/dev/null 2>&1 || true
    done
    tmux_test list-sessions -F '#{session_name}' 2>/dev/null |
        sed -n '/^slurm-logs-/p' |
        while IFS= read -r session; do tmux_test kill-session -t "$session" >/dev/null 2>&1 || true; done
    monitor=$(tmux_test show-options -v -t "$main_session" @slurm_log_monitor_pid 2>/dev/null || true)
    case "$monitor" in ''|*[!0-9]*) ;; *) kill "$monitor" >/dev/null 2>&1 || true ;; esac
    env PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" TMUX_TMPDIR="$tmux_root" \
        SLURM_LOG_CONFIG="$config" "$binary" daemon stop >/dev/null 2>&1 || true
    tmux_test kill-server >/dev/null 2>&1 || true
    rm -rf "$test_root"
}
trap cleanup EXIT HUP INT TERM

now=$(date --iso-8601=seconds)
printf 'offline picker log\n' >"$test_root/job.log"
printf '#!/bin/sh\n#SBATCH --job-name=picker-bank\n' >"$test_root/bank/run.sbatch"
cat >"$fake_bin/squeue" <<'EOF'
#!/bin/sh
printf 'squeue %s\n' "$*" >>"$PICKER_CALL_LOG"
printf '501|RUNNING|grouped|00:01|node|cpu|2026-08-12T10:00:00|501|one.sbatch\n'
printf '500|RUNNING|grouped|00:02|node|cpu|2026-08-12T10:00:00|500|two.sbatch\n'
id=499
while test "$id" -ge 484; do
    printf '%s|RUNNING|job-%s|00:03|node|cpu|2026-08-12T10:00:00|%s|job.sbatch\n' "$id" "$id" "$id"
    id=$((id - 1))
done
printf '600|PENDING|blocked-job|00:00|DependencyNeverSatisfied|cpu|Unknown|1|blocked.sbatch\n'
EOF
cat >"$fake_bin/sacct" <<'EOF'
#!/bin/sh
printf 'sacct %s\n' "$*" >>"$PICKER_CALL_LOG"
printf '700|COMPLETED|alpha-complete|00:03|%s|0:0|1G|cpu=2,mem=2G|cpu\n' "$PICKER_NOW"
printf '701|FAILED|alpha-failed|00:04|%s|1:0|2G|cpu=2,mem=2G|cpu\n' "$PICKER_NOW"
EOF
cat >"$fake_bin/ssh" <<'EOF'
#!/bin/sh
printf 'ssh %s\n' "$*" >>"$PICKER_CALL_LOG"
exit 23
EOF
cat >"$fake_bin/scontrol" <<'EOF'
#!/bin/sh
printf 'scontrol %s\n' "$*" >>"$PICKER_CALL_LOG"
if test "${3:-}" = -o; then
    id=$4
    printf 'JobId=%s JobName=job-%s JobState=RUNNING Reason=None RunTime=00:03:00 TimeLimit=01:00:00 NumNodes=1 NumCPUs=2 Partition=cpu NodeList=node Account=test QOS=normal ReqTRES=cpu=2,mem=2G AllocTRES=cpu=2,mem=2G ExitCode=0:0\n' "$id" "$id"
else
    id=$3
    case "$id" in 500|501) name=grouped ;; *) name=job-$id ;; esac
    printf 'JobId=%s JobName=%s JobState=RUNNING StdOut=%s/job.log\n' "$id" "$name" "$PICKER_ROOT"
fi
EOF
cat >"$fake_bin/sstat" <<'EOF'
#!/bin/sh
printf 'sstat %s\n' "$*" >>"$PICKER_CALL_LOG"
id=$4
printf '%s.batch|2|cpu=2,mem=2G|00:01:00|512M|256M||\n' "$id"
EOF
cat >"$fake_bin/scancel" <<'EOF'
#!/bin/sh
printf 'scancel %s\n' "$*" >>"$PICKER_CALL_LOG"
EOF
chmod 755 "$fake_bin"/*

cat >"$config" <<EOF
{
  "clusters": [
    {"name":"alpha","transport":"local","user":"offline","workingDirectory":"$test_root","accounting":true},
    {"name":"broken","transport":"ssh","user":"offline","sshHost":"broken.invalid","workingDirectory":"/offline","accounting":true}
  ],
  "sbatchBanks": [{"path":"$test_root/bank","name":"Fixtures"}],
  "statePath":"$state"
}
EOF
cat >"$state" <<EOF
{"known":{"alpha:700":"$now","alpha:701":"$now"},"opened":{"alpha:700":"$now","alpha:701":"$now"},"dismissed":{},"baselinedClusters":[],"trackingSchema":2,"autoAddDefault":false,"interactiveJobs":{}}
EOF
chmod 600 "$state"

export TMUX_TMPDIR=$tmux_root
export PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin"
export HOME=$test_root/home
export SLURM_LOG_CONFIG=$config
export SLURM_LOG_STATE=$state
export PICKER_CALL_LOG=$calls
export PICKER_NOW=$now
export PICKER_ROOT=$test_root
mkdir -p "$HOME"

tmux_test new-session -d -s bootstrap sleep 120

start_picker() {
    session=$1
    tmux_test new-session -d -x 160 -y 26 -s "$session" \
        env PATH="$PATH" HOME="$HOME" TMUX_TMPDIR="$TMUX_TMPDIR" \
        SLURM_LOG_CONFIG="$config" SLURM_LOG_STATE="$state" \
        PICKER_CALL_LOG="$calls" PICKER_NOW="$now" PICKER_ROOT="$test_root" \
        "$binary" all --cluster all --refresh 3600
}
screen() { tmux_test capture-pane -p -t "$1" 2>/dev/null || true; }
wait_text() {
    session=$1
    wanted=$2
    attempt=0
    while :; do
        value=$(screen "$session")
        printf '%s\n' "$value" | grep -F "$wanted" >/dev/null && return 0
        attempt=$((attempt + 1))
        test "$attempt" -lt 500 || {
            printf 'Picker did not show %s:\n%s\n' "$wanted" "$value" >&2
            exit 1
        }
        sleep 0.01
    done
}
focused() { screen "$1" | sed -n '/^>/p' | head -1; }
wait_focus_change() {
    session=$1
    before=$2
    attempt=0
    while test "$(focused "$session")" = "$before"; do
        attempt=$((attempt + 1))
        test "$attempt" -lt 200 || { printf 'Focus did not move\n' >&2; exit 1; }
        sleep 0.01
    done
}
wait_focus() {
    session=$1
    wanted=$2
    attempt=0
    while test "$(focused "$session")" != "$wanted"; do
        attempt=$((attempt + 1))
        test "$attempt" -lt 200 || { printf 'Focus did not return\n' >&2; exit 1; }
        sleep 0.01
    done
}

start_picker "$main_session"
wait_text "$main_session" 'Cluster [ ALL ]'
wait_text "$main_session" 'blocked: 1 (b to show)'

# Every navigation alias and boundary key moves to the intended row.
first=$(focused "$main_session")
tmux_test send-keys -t "$main_session" Down
wait_focus_change "$main_session" "$first"
tmux_test send-keys -t "$main_session" Up
wait_focus "$main_session" "$first"
tmux_test send-keys -t "$main_session" j
wait_focus_change "$main_session" "$first"
tmux_test send-keys -t "$main_session" k
wait_focus "$main_session" "$first"
tmux_test send-keys -t "$main_session" End
wait_focus_change "$main_session" "$first"
tmux_test send-keys -t "$main_session" Home
wait_focus "$main_session" "$first"
tmux_test send-keys -t "$main_session" NPage
wait_focus_change "$main_session" "$first"
tmux_test send-keys -t "$main_session" PPage
wait_focus "$main_session" "$first"
tmux_test send-keys -t "$main_session" G
wait_focus_change "$main_session" "$first"
tmux_test send-keys -t "$main_session" g
wait_focus "$main_session" "$first"

# Search, group expand/collapse aliases, group selection, select-all, and clear.
tmux_test send-keys -t "$main_session" / grouped Enter
wait_text "$main_session" 'search="grouped"'
wait_text "$main_session" '2 runs  ·  grouped'
! screen "$main_session" | grep -E '[[:space:]]50[01][[:space:]]' >/dev/null
tmux_test send-keys -t "$main_session" l
wait_text "$main_session" '  501'
wait_text "$main_session" '  500'
tmux_test send-keys -t "$main_session" h
attempt=0
while screen "$main_session" | grep -E '[[:space:]]50[01][[:space:]]' >/dev/null; do
    attempt=$((attempt + 1)); test "$attempt" -lt 200; sleep 0.01
done
tmux_test send-keys -t "$main_session" Space
wait_text "$main_session" '2 selected'
tmux_test send-keys -t "$main_session" c
wait_text "$main_session" '0 selected'
tmux_test send-keys -t "$main_session" v
wait_text "$main_session" '2 selected'
tmux_test send-keys -t "$main_session" c Escape
wait_text "$main_session" 'job-499'

# All view and behavior toggles provide visible feedback and update state.
tmux_test send-keys -t "$main_session" b
wait_text "$main_session" 'Blocked and interactive jobs shown'
wait_text "$main_session" blocked-job
tmux_test send-keys -t "$main_session" b
wait_text "$main_session" 'Blocked and interactive jobs hidden'
tmux_test send-keys -t "$main_session" w
wait_text "$main_session" 'Scheduler notices expanded'
tmux_test send-keys -t "$main_session" w
wait_text "$main_session" 'Scheduler notices collapsed'
tmux_test send-keys -t "$main_session" W
wait_text "$main_session" 'Warnings included in log panes'
tmux_test send-keys -t "$main_session" W
wait_text "$main_session" 'Warnings hidden in log panes'
tmux_test send-keys -t "$main_session" A
wait_text "$main_session" 'Auto-add enabled'
grep -F '"autoAddDefault":true' "$state" >/dev/null
tmux_test send-keys -t "$main_session" A
wait_text "$main_session" 'Auto-add disabled'
grep -F '"autoAddDefault":false' "$state" >/dev/null
tmux_test send-keys -t "$main_session" r
wait_text "$main_session" 'Scheduler refreshed'
tmux_test send-keys -t "$main_session" o
wait_text "$main_session" 'Recent completed jobs shown'
wait_text "$main_session" alpha-complete
tmux_test send-keys -t "$main_session" o
wait_text "$main_session" 'Recent completed jobs hidden'
tmux_test send-keys -t "$main_session" a
wait_text "$main_session" 'Accounting archive shown'
wait_text "$main_session" alpha-complete

# Dismiss targets the filtered terminal job and persists it in the ledger.
tmux_test send-keys -t "$main_session" / alpha-complete Enter
wait_text "$main_session" 'search="alpha-complete"'
tmux_test send-keys -t "$main_session" d
attempt=0
while screen "$main_session" | grep -E '[[:space:]]alpha[[:space:]]+700.*alpha-complete' >/dev/null; do
    attempt=$((attempt + 1)); test "$attempt" -lt 200; sleep 0.01
done
grep -F '"dismissed":{"alpha:700"' "$state" >/dev/null
tmux_test send-keys -t "$main_session" Escape a
wait_text "$main_session" 'Live jobs shown'

# Details on a concrete job makes the expected live calls and returns to the
# picker; `s` enters and exits the script bank without corrupting the screen.
tmux_test send-keys -t "$main_session" / job-499 Enter i
attempt=0
until grep -F 'scontrol show job -o 499' "$calls" >/dev/null 2>&1; do
    attempt=$((attempt + 1)); test "$attempt" -lt 500; sleep 0.01
done
wait_text "$main_session" 'alpha:499  job-499'
wait_text "$main_session" 'q/Esc/Enter close'
tmux_test send-keys -t "$main_session" q
wait_text "$main_session" 'search="job-499"'
tmux_test send-keys -t "$main_session" Escape
attempt=0
while screen "$main_session" | grep -F 'search="job-499"' >/dev/null; do
    attempt=$((attempt + 1)); test "$attempt" -lt 200; sleep 0.01
done
tmux_test send-keys -t "$main_session" s
wait_text "$main_session" 'SBATCH BANK'
tmux_test send-keys -t "$main_session" s
wait_text "$main_session" 'Cluster [ ALL ]'

# Search cancellation and applied-search clearing are distinct Esc paths.
tmux_test send-keys -t "$main_session" / ignored Escape
wait_text "$main_session" job-499
tmux_test send-keys -t "$main_session" / no-such-row Enter
wait_text "$main_session" 'search="no-such-row"'
tmux_test send-keys -t "$main_session" Escape
wait_text "$main_session" job-499

# The static reference supports all navigation families and returns cleanly.
tmux_test send-keys -t "$main_session" '?'
wait_text "$main_session" 'slurm-log command reference'
wait_text "$main_session" NAVIGATION
tmux_test send-keys -t "$main_session" Down j NPage End G
wait_text "$main_session" 'Right-click'
tmux_test send-keys -t "$main_session" PPage Home g
wait_text "$main_session" NAVIGATION
tmux_test send-keys -t "$main_session" '?'
wait_text "$main_session" 'Cluster [ ALL ]'

# Main-picker q exits without opening a workspace.
tmux_test send-keys -t "$main_session" q
attempt=0
while tmux_test has-session -t "$main_session" 2>/dev/null; do
    attempt=$((attempt + 1)); test "$attempt" -lt 300; sleep 0.01
done

# Enter opens exactly the selected collapsed group's two jobs in a workspace.
start_picker "$open_session"
wait_text "$open_session" 'Cluster [ ALL ]'
tmux_test send-keys -t "$open_session" / grouped Enter Space Enter
attempt=0
workspace=
while test -z "$workspace"; do
    workspace=$(tmux_test list-sessions -F '#{session_name}' | sed -n '/^slurm-logs-/{p;q;}')
    attempt=$((attempt + 1)); test "$attempt" -lt 500; sleep 0.01
done
attempt=0
while :; do
    ids=$(tmux_test list-panes -t "$workspace" -F '#{@slurm_log_job_id}' | sort | tr '\n' ' ')
    test "$ids" = '500 501 ' && break
    attempt=$((attempt + 1)); test "$attempt" -lt 500 || {
        printf 'Selection opened wrong panes: %s\n' "$ids" >&2; exit 1;
    }
    sleep 0.01
done
# The original picker remains alive and pre-renders a fresh list behind the
# workspace. Closing that workspace therefore returns to the list, not a shell.
wait_text "$open_session" 'Cluster [ ALL ]'
tmux_test kill-session -t "$workspace"
tmux_test has-session -t "$open_session"
wait_text "$open_session" grouped

# Exit the returned picker normally so destructors and instrumented profiles
# flush before the private tmux server is removed.
tmux_test send-keys -t "$open_session" q
attempt=0
while tmux_test has-session -t "$open_session" 2>/dev/null; do
    attempt=$((attempt + 1)); test "$attempt" -lt 300; sleep 0.01
done

printf 'picker_controls: ok (all documented picker keys + exact group open; fully offline)\n'
