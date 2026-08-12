#!/bin/sh
# Fully offline regression for pane-focus metadata and the persistent status.
# A fake local scontrol resolves the name; no SSH or Slurm service is contacted.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=$project_dir/target/release/slurm-log
test_root=$(mktemp -d)
tmux_root=$test_root/tmux
tmux_test() {
    env TMUX_TMPDIR="$tmux_root" tmux "$@"
}
cleanup() {
    tmux_test kill-server >/dev/null 2>&1 || true
    rm -rf "$test_root"
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$test_root/bin" "$test_root/state" "$tmux_root"
chmod 700 "$tmux_root"
log=$test_root/job.log
printf 'offline log\n' >"$log"
cat >"$test_root/bin/scontrol" <<EOF
#!/bin/sh
printf 'JobId=4242424242 JobName=focus-training StdOut=$log\n'
EOF
chmod 755 "$test_root/bin/scontrol"
config=$test_root/config.json
cat >"$config" <<EOF
{"clusters":[{"name":"local","transport":"local","user":"offline","workingDirectory":"$test_root","accounting":false}],"statePath":"$test_root/state/state.json"}
EOF

# Use a private tmux server so its environment includes the fake scheduler PATH
# and the test cannot inspect or modify a real slurm-log workspace.
env TMUX_TMPDIR="$tmux_root" PATH="$test_root/bin:/usr/local/bin:/usr/bin:/bin" \
    tmux new-session -d -s bootstrap sleep 60
tmux_test set-environment -g SLURM_LOG_CONFIG "$config"

# Attaching without a terminal fails only after the workspace and its hooks are
# created. The follower remains isolated in the detached test session.
env -u TMUX TMUX_TMPDIR="$tmux_root" PATH="$test_root/bin:/usr/local/bin:/usr/bin:/bin" \
    SLURM_LOG_CONFIG="$config" "$binary" local 4242424242 \
    >"$test_root/open.out" 2>"$test_root/open.err" || true

attempt=0
while :; do
    pane=$(tmux_test list-panes -a \
        -F '#{session_name}|#{pane_id}|#{@slurm_log_cluster}|#{@slurm_log_job_id}|#{@slurm_log_job_name}' |
        awk -F '|' '$3 == "local" && $4 == "4242424242" && $1 ~ /^slurm-logs-/ { print; exit }')
    if test -n "$pane" && test "${pane##*|}" = focus-training; then
        break
    fi
    attempt=$((attempt + 1))
    test "$attempt" -lt 500 || {
        printf 'Focus metadata was not attached to the fake job pane: %s\n' "$pane" >&2
        test ! -s "$test_root/open.err" || cat "$test_root/open.err" >&2
        test -z "$pane" || tmux_test capture-pane -p -S -50 -t "$(printf '%s\n' "$pane" | cut -d '|' -f 2)" >&2
        exit 1
    }
    sleep 0.01
done
session=${pane%%|*}
# Pane switches must not use display-message: its message-style briefly paints
# a yellow bar over the persistent green status in many tmux configurations.
hook=$(tmux_test show-hooks -t "$session" after-select-pane 2>/dev/null || true)
! printf '%s\n' "$hook" | grep -F 'display-message' >/dev/null

# The same identity remains in the green status bar after the short focus
# message expires; tmux's generic session/window/binary labels are suppressed.
test -z "$(tmux_test show-options -v -t "$session" status-left)"
test -z "$(tmux_test show-options -v -t "$session" status-right)"
effective_style=$(tmux_test show-options -Av -t "$session" status-style)
case "$effective_style" in
    *fg=colour0*bg=colour2*|*bg=colour2*fg=colour0*) ;;
    *) printf 'Unexpected status style: %s\n' "$effective_style" >&2; exit 1 ;;
esac
effective_current_style=$(tmux_test show-options -Av -t "$session" window-status-current-style)
case "$effective_current_style" in
    *fg=colour0*bg=colour2*|*bg=colour2*fg=colour0*) ;;
    *) printf 'Unexpected current status style: %s\n' "$effective_current_style" >&2; exit 1 ;;
esac
status_format=$(tmux_test show-options -v -t "$session" window-status-current-format)
printf '%s\n' "$status_format" | grep -F '#{@slurm_log_job_name}' >/dev/null
printf '%s\n' "$status_format" | grep -F '#{@slurm_log_job_id}' >/dev/null
expanded=$(tmux_test display-message -p -t "$(printf '%s\n' "$pane" | cut -d '|' -f 2)" \
    '#{E:window-status-current-format}')
test "$expanded" = 'focus-training · job 4242424242'
printf '%s\n' "$status_format" | grep -F 'slurm-log-rust' >/dev/null && exit 1

printf 'focus_status: ok (black-on-green, no transient message; fully offline)\n'
