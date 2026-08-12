#!/bin/sh
# Fully offline end-to-end regression for tmux workspace configuration and
# every documented prefix/mouse control. Uses a private tmux server.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=${SLURM_LOG_TEST_BINARY:-$project_dir/target/release/slurm-log}
test_root=$(mktemp -d)
case "$test_root" in /tmp/*) ;; *) exit 1 ;; esac
tmux_root=$test_root/tmux
fake_bin=$test_root/bin
config=$test_root/config.json
state=$test_root/state/state.json
mkdir -p "$tmux_root" "$fake_bin" "$test_root/state" "$test_root/home"
chmod 700 "$tmux_root"

tmux_test() { env TMUX_TMPDIR="$tmux_root" tmux "$@"; }
cleanup() {
    monitor=${monitor:-}
    case "$monitor" in ''|*[!0-9]*) ;; *) kill "$monitor" >/dev/null 2>&1 || true ;; esac
    env TMUX_TMPDIR="$tmux_root" PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" \
        SLURM_LOG_CONFIG="$config" "$binary" daemon stop >/dev/null 2>&1 || true
    tmux_test kill-server >/dev/null 2>&1 || true
    rm -rf "$test_root"
}
trap cleanup EXIT HUP INT TERM

printf 'workspace log\n' >"$test_root/job.log"
printf 'auto-added log\n' >"$test_root/job-102.log"
phase=$test_root/queue-phase
monitor_seen=$test_root/monitor-seen
printf 'base\n' >"$phase"
cat >"$fake_bin/scontrol" <<'EOF'
#!/bin/sh
case "$*" in
    'show job 101')
        printf 'JobId=101 JobName=workspace-job JobState=RUNNING StdOut=%s/job.log\n' "$WORKSPACE_ROOT"
        ;;
    'show job 102')
        printf 'JobId=102 JobName=auto-start JobState=RUNNING StdOut=%s/job-102.log\n' "$WORKSPACE_ROOT"
        ;;
    *) exit 31 ;;
esac
EOF
cat >"$fake_bin/squeue" <<'EOF'
#!/bin/sh
printf '101|RUNNING|workspace-job|00:01|node|cpu|2026-08-12T10:00:00|100|run.sbatch\n'
case "$(cat "$WORKSPACE_PHASE")" in
    pending)
        printf '102|PENDING|auto-start|00:00|Resources|cpu|Unknown|100|auto.sbatch\n'
        printf '103|PENDING|early-fail|00:00|Resources|cpu|Unknown|100|fail.sbatch\n'
        printf '104|PENDING|vanishing|00:00|Resources|cpu|Unknown|100|gone.sbatch\n'
        : >"$WORKSPACE_MONITOR_SEEN"
        ;;
    running)
        printf '102|RUNNING|auto-start|00:01|node|cpu|2026-08-12T10:01:00|100|auto.sbatch\n'
        printf '103|FAILED|early-fail|00:00|launch failed|cpu|Unknown|100|fail.sbatch\n'
        ;;
esac
EOF
cat >"$fake_bin/sacct" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod 755 "$fake_bin/scontrol" "$fake_bin/squeue" "$fake_bin/sacct"
cat >"$config" <<EOF
{"clusters":[{"name":"alpha","transport":"local","user":"offline","workingDirectory":"$test_root","accounting":true}],"statePath":"$state"}
EOF

export TMUX_TMPDIR=$tmux_root
export PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin"
export HOME=$test_root/home
export WORKSPACE_ROOT=$test_root
export WORKSPACE_PHASE=$phase
export WORKSPACE_MONITOR_SEEN=$monitor_seen
export SLURM_LOG_CONFIG=$config
export SLURM_LOG_STATE=$state

# Bootstrap only the isolated server. A non-TTY attach fails after the tested
# application has created and configured its detached workspace.
tmux_test new-session -d -s bootstrap sleep 120
timeout -k 1 2 env -u TMUX "$binary" alpha 101 \
    >"$test_root/open.out" 2>"$test_root/open.err" || true
session=$(tmux_test list-sessions -F '#{session_name}' | sed -n '/^slurm-logs-/{p;q;}')
test -n "$session"

attempt=0
while :; do
    pane=$(tmux_test list-panes -t "$session" \
        -F '#{pane_id}|#{@slurm_log_cluster}|#{@slurm_log_job_id}|#{@slurm_log_job_name}' | head -1)
    test "${pane#*|}" = 'alpha|101|workspace-job' && break
    attempt=$((attempt + 1))
    test "$attempt" -lt 300 || { printf 'workspace metadata never settled: %s\n' "$pane" >&2; exit 1; }
    sleep 0.01
done

# Workspace behavior and the persistent black-on-green job identity.
test "$(tmux_test show-options -v -t "$session" mouse)" = on
test "$(tmux_test show-options -v -t "$session" history-limit)" = 50000
test "$(tmux_test show-options -wv -t "$session" remain-on-exit)" = on
test "$(tmux_test show-options -v -t "$session" bell-action)" = any
test "$(tmux_test show-options -v -t "$session" visual-bell)" = off
test -z "$(tmux_test show-options -v -t "$session" status-left)"
test -z "$(tmux_test show-options -v -t "$session" status-right)"
test "$(tmux_test show-options -sv set-clipboard)" = on
style=$(tmux_test show-options -Av -t "$session" status-style)
case "$style" in *fg=colour0*bg=colour2*|*bg=colour2*fg=colour0*) ;; *) exit 1 ;; esac
format=$(tmux_test show-options -v -t "$session" window-status-current-format)
printf '%s\n' "$format" | grep -F '#{@slurm_log_job_name}' >/dev/null
printf '%s\n' "$format" | grep -F '#{@slurm_log_job_id}' >/dev/null
hook=$(tmux_test show-hooks -t "$session" after-select-pane 2>/dev/null || true)
! printf '%s\n' "$hook" | grep -F display-message >/dev/null

# Prefix commands: picker aliases, details, auto-add, smart close, close pane,
# zoom, and removal of the legacy uppercase close key.
for key in j a; do
    binding=$(tmux_test list-keys -T prefix "$key")
    printf '%s\n' "$binding" | grep -F 'display-popup' >/dev/null
    printf '%s\n' "$binding" | grep -F 'pick-add' >/dev/null
    printf '%s\n' "$binding" | grep -F 'refresh-client' >/dev/null
done
tmux_test list-keys -T prefix i | grep -F 'toggle-details' >/dev/null
tmux_test list-keys -T prefix A | grep -F 'toggle-auto' >/dev/null
q_binding=$(tmux_test list-keys -T prefix q)
printf '%s\n' "$q_binding" | grep -F 'single-pane' >/dev/null
printf '%s\n' "$q_binding" | grep -F 'confirm-before' >/dev/null
x_binding=$(tmux_test list-keys -T prefix x)
printf '%s\n' "$x_binding" | grep -F 'close-pane' >/dev/null
! printf '%s\n' "$x_binding" | grep -F 'confirm-before' >/dev/null
tmux_test list-keys -T prefix z | grep -F 'resize-pane -Z' >/dev/null
if tmux_test list-keys -T prefix Q >/dev/null 2>&1; then
    printf 'legacy Ctrl-b Q binding still exists\n' >&2
    exit 1
fi

# Ctrl-b x protects the final log pane with visible feedback, closes an extra
# log immediately, and also closes an auxiliary details pane without a prompt.
first_pane=${pane%%|*}
"$binary" close-pane "$session" "$first_pane"
tmux_test has-session -t "$session"
tmux_test display-message -p -t "$session" '#{pane_id}' | grep -Fx "$first_pane" >/dev/null
extra_pane=$(tmux_test split-window -d -P -F '#{pane_id}' -t "$session" sleep 120)
tmux_test set-option -p -t "$extra_pane" @slurm_log_cluster alpha
tmux_test set-option -p -t "$extra_pane" @slurm_log_job_id 999
"$binary" close-pane "$session" "$extra_pane"
! tmux_test list-panes -t "$session" -F '#{pane_id}' | grep -Fx "$extra_pane" >/dev/null
detail_pane=$(tmux_test split-window -d -P -F '#{pane_id}' -t "$session" sleep 120)
tmux_test set-option -p -t "$detail_pane" @slurm_log_detail_parent "$first_pane"
"$binary" close-pane "$session" "$detail_pane"
! tmux_test list-panes -t "$session" -F '#{pane_id}' | grep -Fx "$detail_pane" >/dev/null

# Mouse behavior: drag preserves selection, right-click copies from anywhere
# with a 1.5 s toast, and left-click cancels selection.
for table in copy-mode copy-mode-vi; do
    tmux_test list-keys -T "$table" MouseDragEnd1Pane | grep -F stop-selection >/dev/null
    tmux_test list-keys -T "$table" MouseDown3Pane | grep -F copy-selection-and-cancel >/dev/null
    toast=$(tmux_test list-keys -T "$table" MouseUp3Pane)
    printf '%s\n' "$toast" | grep -F 'Copied to clipboard' >/dev/null
    printf '%s\n' "$toast" | grep -F 1500 >/dev/null
    tmux_test list-keys -T "$table" MouseUp1Pane | grep -F 'send-keys -X cancel' >/dev/null
done
root_toast=$(tmux_test list-keys -T root MouseUp3Pane)
printf '%s\n' "$root_toast" | grep -F 'Copied to clipboard' >/dev/null
printf '%s\n' "$root_toast" | grep -F 1500 >/dev/null

# Auto-add changes both workspace and persistent defaults, and starts one
# monitor process. Toggling it back off is observable immediately.
test "$(tmux_test show-options -v -t "$session" @slurm_log_auto_add)" = off
printf 'pending\n' >"$phase"
"$binary" toggle-auto "$session"
test "$(tmux_test show-options -v -t "$session" @slurm_log_auto_add)" = on
grep -F '"autoAddDefault":true' "$state" >/dev/null
monitor=$(tmux_test show-options -v -t "$session" @slurm_log_monitor_pid)
case "$monitor" in ''|*[!0-9]*) exit 1 ;; esac
attempt=0
while ! test -f "$monitor_seen"; do
    attempt=$((attempt + 1))
    test "$attempt" -lt 300 || { printf 'auto monitor did not take its initial snapshot\n' >&2; exit 1; }
    sleep 0.01
done
printf 'running\n' >"$phase"
attempt=0
while ! tmux_test list-panes -t "$session" \
    -F '#{@slurm_log_cluster}:#{@slurm_log_job_id}' | grep -Fx 'alpha:102' >/dev/null; do
    attempt=$((attempt + 1))
    test "$attempt" -lt 700 || { printf 'auto monitor did not add the started job\n' >&2; exit 1; }
    sleep 0.01
done
# A second scheduler frame confirms that pending job 104 vanished. The monitor
# then observes the disabled option and exits normally, preserving coverage.
sleep 3.2
"$binary" toggle-auto "$session"
test "$(tmux_test show-options -v -t "$session" @slurm_log_auto_add)" = off
grep -F '"autoAddDefault":false' "$state" >/dev/null
sleep 3.2
auto_pane=$(tmux_test list-panes -t "$session" \
    -F '#{pane_id}|#{@slurm_log_job_id}' | sed -n 's/|102$//p')
test -n "$auto_pane"
tmux_test kill-pane -t "$auto_pane"

# With one log pane, execute the same if-shell branches installed for Ctrl-b q;
# tmux send-keys writes into the PTY and intentionally does not emulate a
# client's prefix-key parser.
tmux_test if-shell "$binary single-pane $session" \
    "kill-session -t $session" \
    "confirm-before -p 'Close the entire slurm-log workspace? (y/n)' 'kill-session -t $session'"
attempt=0
while tmux_test has-session -t "$session" 2>/dev/null; do
    attempt=$((attempt + 1)); test "$attempt" -lt 300; sleep 0.01
done

# Workspace management acts only on slurm-log sessions in a real tmux server.
tmux_test new-session -d -s ordinary sleep 120
tmux_test new-session -d -s slurm-logs-control-a sleep 120
tmux_test new-session -d -s slurm-logs-control-b sleep 120
"$binary" sessions >"$test_root/sessions"
grep -Fx slurm-logs-control-a "$test_root/sessions" >/dev/null
grep -Fx slurm-logs-control-b "$test_root/sessions" >/dev/null
! grep -Fx ordinary "$test_root/sessions" >/dev/null
"$binary" close slurm-logs-control-a
! tmux_test has-session -t slurm-logs-control-a 2>/dev/null
"$binary" close all
! tmux_test has-session -t slurm-logs-control-b 2>/dev/null
tmux_test has-session -t ordinary

printf 'workspace_controls: ok (options, prefix/mouse keys, auto-add, lifecycle; fully offline)\n'
