#!/bin/sh
# Offline PTY regression for cluster tabs in both picker renderers. Fake local
# and SSH scheduler commands provide disjoint jobs; no network or SLURM service
# is contacted.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=${SLURM_LOG_TEST_BINARY:-$project_dir/target/release/slurm-log}
test_root=$(mktemp -d)
main_session=slurm-log-cluster-main-$$
popup_session=slurm-log-cluster-popup-$$

cleanup() {
    tmux kill-session -t "$main_session" >/dev/null 2>&1 || true
    tmux kill-session -t "$popup_session" >/dev/null 2>&1 || true
    env PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" \
        HOME="$test_root/home" \
        SLURM_LOG_CONFIG="$config" \
        SLURM_LOG_STATE="$test_root/state/state.json" \
        "$binary" daemon stop >/dev/null 2>&1 || true
    tmux kill-server >/dev/null 2>&1 || true
    rm -rf "$test_root"
}
trap cleanup EXIT HUP INT TERM

fake_bin=$test_root/bin
config=$test_root/config.json
call_log=$test_root/scheduler-calls
mkdir -p "$fake_bin" "$test_root/home" "$test_root/state" "$test_root/tmux"
chmod 700 "$test_root/tmux"
export TMUX_TMPDIR=$test_root/tmux
tmux new-session -d -s cluster-tabs-bootstrap sleep 120

cat >"$fake_bin/squeue" <<'EOF'
#!/bin/sh
printf 'squeue %s\n' "$*" >>"$OFFLINE_CALL_LOG"
printf '101|RUNNING|alpha-only-job|00:01|alpha-node|cpu|2026-08-12T10:00:00|1000\n'
EOF

cat >"$fake_bin/ssh" <<'EOF'
#!/bin/sh
printf 'ssh %s\n' "$*" >>"$OFFLINE_CALL_LOG"
case "$*" in
    *beta.invalid*'squeue -h'*)
        printf '202|RUNNING|beta-only-job|00:02|beta-node|gpu|2026-08-12T10:00:00|2000\n999|PENDING|blocked-open-job|00:00|DependencyNeverSatisfied|gpu|Unknown|100\n'
        ;;
    *beta.invalid*scancel*) : ;;
    *)
        printf 'unexpected fake SSH request: %s\n' "$*" >&2
        exit 23
        ;;
esac
EOF
cat >"$fake_bin/scancel" <<'EOF'
#!/bin/sh
printf 'scancel %s\n' "$*" >>"$OFFLINE_CALL_LOG"
EOF
chmod 755 "$fake_bin/squeue" "$fake_bin/ssh" "$fake_bin/scancel"

cat >"$config" <<EOF
{"clusters":[{"name":"alpha","transport":"local","user":"offline","sshHost":"","workingDirectory":"$test_root","accounting":false},{"name":"beta","transport":"ssh","user":"offline","sshHost":"beta.invalid","workingDirectory":"/offline","accounting":false}],"statePath":"$test_root/state/state.json"}
EOF

jobs_json=$test_root/jobs.json
env PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" \
    HOME="$test_root/home" \
    SLURM_LOG_CONFIG="$config" \
    SLURM_LOG_STATE="$test_root/state/state.json" \
    OFFLINE_CALL_LOG="$call_log" \
    "$binary" json --cluster all >"$jobs_json"
grep -F alpha-only-job "$jobs_json" >/dev/null
grep -F beta-only-job "$jobs_json" >/dev/null

start_picker() {
    session=$1
    width=$2
    height=$3
    shift 3
    tmux new-session -d -x "$width" -y "$height" -s "$session" \
        'while :; do sleep 60; done'
    # An existing tmux server may otherwise inherit an attached client's size
    # and ignore new-session's detached dimensions. Size the pane before the
    # picker starts so terminal reflow cannot create false stale-row failures.
    tmux set-option -t "$session" window-size manual
    tmux resize-window -t "$session" -x "$width" -y "$height"
    if test -n "${tag_cluster:-}"; then
        tmux set-option -p -t "$session" @slurm_log_cluster "$tag_cluster"
        tmux set-option -p -t "$session" @slurm_log_job_id "$tag_job"
    fi
    tmux respawn-pane -k -t "$session" \
        env PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" \
        HOME="$test_root/home" \
        SLURM_LOG_CONFIG="$config" \
        SLURM_LOG_STATE="$test_root/state/state.json" \
        OFFLINE_CALL_LOG="$call_log" \
        "$@"
}

wait_for_view() {
    session=$1
    cluster=$2
    present=$3
    absent=$4
    selected=$5
    attempt=0
    while :; do
        screen=$(tmux capture-pane -p -t "$session" 2>/dev/null || true)
        if { printf '%s\n' "$screen" | grep -F "[Tab $cluster]" >/dev/null \
                || printf '%s\n' "$screen" | grep -F "[Tab] $cluster" >/dev/null; } \
            && printf '%s\n' "$screen" | grep -F "$present" >/dev/null \
            && printf '%s\n' "$screen" | grep -F "$selected selected" >/dev/null \
            && { test -z "$absent" || ! printf '%s\n' "$screen" | grep -F "$absent" >/dev/null; }
        then
            return 0
        fi
        attempt=$((attempt + 1))
        test "$attempt" -lt 500 || {
            tmux send-keys -t "$session" w >/dev/null 2>&1 || true
            sleep 0.05
            screen=$(tmux capture-pane -p -t "$session" 2>/dev/null || true)
            printf 'Cluster view did not settle (%s, expected %s):\n%s\n' "$session" "$cluster" "$screen" >&2
            printf 'Fake scheduler calls:\n' >&2
            test ! -f "$call_log" || cat "$call_log" >&2
            exit 1
        }
        sleep 0.01
    done
}

wait_for_screen_text() {
    session=$1
    wanted=$2
    attempt=0
    while :; do
        screen=$(tmux capture-pane -p -t "$session" 2>/dev/null || true)
        printf '%s\n' "$screen" | grep -F "$wanted" >/dev/null && return 0
        attempt=$((attempt + 1))
        test "$attempt" -lt 150 || {
            printf 'Picker did not show %s:\n%s\n' "$wanted" "$screen" >&2
            exit 1
        }
        sleep 0.01
    done
}

# Full-size main picker: forward and backward cycling must repaint the rows.
start_picker "$main_session" 120 20 "$binary" all --cluster all --refresh 3600
wait_for_view "$main_session" ALL alpha-only-job '' 0
screen=$(tmux capture-pane -p -t "$main_session")
printf '%s\n' "$screen" | grep -F beta-only-job >/dev/null
printf '%s\n' "$screen" | grep -F 'Space mark' >/dev/null
printf '%s\n' "$screen" | grep -F '[A AUTO OFF]' >/dev/null

# Stop waits for an explicit decision (unrelated keys do not cancel it), sends
# scancel, and reports success instead of silently returning to the list.
tmux send-keys -t "$main_session" x
wait_for_screen_text "$main_session" 'STOP 1 ACTIVE JOB(S)?'
tmux send-keys -t "$main_session" a
wait_for_screen_text "$main_session" 'STOP 1 ACTIVE JOB(S)?'
tmux send-keys -t "$main_session" y
wait_for_screen_text "$main_session" 'Stop requested for 1 job(s)'
grep -E 'scancel|ssh .*scancel' "$call_log" >/dev/null

# View toggles must change both the state and the listing, and provide a clear
# 1.5-second confirmation instead of silently repainting.
tmux send-keys -t "$main_session" b
wait_for_screen_text "$main_session" 'Blocked and interactive jobs shown'
wait_for_screen_text "$main_session" 'blocked-open-job'
tmux send-keys -t "$main_session" b
wait_for_screen_text "$main_session" 'Blocked and interactive jobs hidden'
screen=$(tmux capture-pane -p -t "$main_session")
! printf '%s\n' "$screen" | grep -F blocked-open-job >/dev/null
sleep 0.5
screen=$(tmux capture-pane -p -t "$main_session")
printf '%s\n' "$screen" | grep -F 'Blocked and interactive jobs hidden' >/dev/null
sleep 1.1
screen=$(tmux capture-pane -p -t "$main_session")
! printf '%s\n' "$screen" | grep -F 'Blocked and interactive jobs hidden' >/dev/null

# Ordinary job picking remains scoped to the visible cluster tab.
tmux send-keys -t "$main_session" Space
wait_for_view "$main_session" ALL alpha-only-job '' 1
tmux send-keys -t "$main_session" Tab
wait_for_view "$main_session" alpha alpha-only-job beta-only-job 0
tmux send-keys -t "$main_session" Tab
wait_for_view "$main_session" beta beta-only-job alpha-only-job 0
tmux send-keys -t "$main_session" BTab
wait_for_view "$main_session" alpha alpha-only-job beta-only-job 0
tmux send-keys -t "$main_session" q

# Ctrl-b j uses the compact popup renderer. Cycle backwards from ALL, then
# forwards through ALL to alpha, checking that old colored rows are erased.
# The tagged beta pane is dependency-blocked, so the ordinary live filter hides
# it. Because it is already open, Ctrl-b j must retain its real PENDING metadata
# (and orange color) instead of replacing it with a red OPEN placeholder.
tag_cluster=beta
tag_job=999
start_picker "$popup_session" 80 16 env SLURM_LOG_POPUP=1 \
    "$binary" pick-add "$popup_session" --cluster all --refresh 3600
unset tag_cluster tag_job
wait_for_view "$popup_session" ALL beta-only-job '' 1
screen=$(tmux capture-pane -p -t "$popup_session")
printf '%s\n' "$screen" | grep -F 999 >/dev/null
printf '%s\n' "$screen" | grep -F PENDING >/dev/null
! printf '%s\n' "$screen" | grep -F OPEN >/dev/null
tmux send-keys -t "$popup_session" BTab
wait_for_view "$popup_session" beta beta-only-job alpha-only-job 1
screen=$(tmux capture-pane -p -t "$popup_session")
printf '%s\n' "$screen" | grep -F 999 >/dev/null
tmux send-keys -t "$popup_session" Tab
wait_for_view "$popup_session" ALL alpha-only-job '' 1
screen=$(tmux capture-pane -p -t "$popup_session")
printf '%s\n' "$screen" | grep -F beta-only-job >/dev/null
tmux send-keys -t "$popup_session" Tab
wait_for_view "$popup_session" alpha alpha-only-job 999 1
screen=$(tmux capture-pane -p -t "$popup_session")
! printf '%s\n' "$screen" | grep -F beta-only-job >/dev/null
# Returning to beta must restore the marker for its already-open pane without
# requiring the user to select it again.
tmux send-keys -t "$popup_session" BTab
wait_for_view "$popup_session" ALL alpha-only-job '' 1
tmux send-keys -t "$popup_session" BTab
wait_for_view "$popup_session" beta beta-only-job alpha-only-job 1
screen=$(tmux capture-pane -p -t "$popup_session")
printf '%s\n' "$screen" | grep -E '^.?>?\*.*999' >/dev/null

# The popup recomposes rather than clipping as its window is squeezed. The
# selection and cluster scope survive every resize, and widening erases all
# rows belonging only to the narrow layout.
tmux send-keys -t "$popup_session" Tab
wait_for_view "$popup_session" ALL alpha-only-job '' 1
tmux resize-window -t "$popup_session" -x 63 -y 16
wait_for_screen_text "$popup_session" 'SCOPE   [Tab] ALL'
screen=$(tmux capture-pane -p -t "$popup_session")
printf '%s\n' "$screen" | grep -F 'STATE   [A] AUTO OFF' >/dev/null
printf '%s\n' "$screen" | grep -F 'FILTER  [b] 1 blocked hidden' >/dev/null
printf '%s\n' "$screen" | grep -F 'CLUSTER JOB ID' >/dev/null
! printf '%s\n' "$screen" | grep -F ELAPSED >/dev/null
printf '%s\n' "$screen" | grep -F '1 selected' >/dev/null

tmux resize-window -t "$popup_session" -x 80 -y 16
wait_for_screen_text "$popup_session" 'STATUS  [Tab] ALL'
screen=$(tmux capture-pane -p -t "$popup_session")
printf '%s\n' "$screen" | grep -F ELAPSED >/dev/null
! printf '%s\n' "$screen" | grep -F 'SCOPE   ' >/dev/null
! printf '%s\n' "$screen" | grep -F 'STATE   ' >/dev/null

tmux resize-window -t "$popup_session" -x 43 -y 16
wait_for_screen_text "$popup_session" 'Window too small'
screen=$(tmux capture-pane -p -t "$popup_session")
! printf '%s\n' "$screen" | grep -F alpha-only-job >/dev/null
tmux send-keys -t "$popup_session" q

printf 'cluster_tabs: ok (main + responsive Ctrl-b j repaint, resize, tabs, selection; fully offline)\n'
