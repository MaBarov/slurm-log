#!/bin/sh
# Offline regression test for Enter-to-close inside a real tmux PTY. Scheduler
# and SSH behavior is entirely mocked; this never contacts a cluster.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=${SLURM_LOG_TEST_BINARY:-$project_dir/target/release/slurm-log}
test_root=$(mktemp -d)
session=slurm-log-close-test-$$
socket=slurm-log-close-test-$$
tmux_binary=$(command -v tmux)
tmux_dir=$(dirname -- "$tmux_binary")
tmux_test() {
    "$tmux_binary" -L "$socket" "$@"
}
fail_stage() {
    stage=$1
    shift
    printf '%s\n' "$*" >&2
    if [ "${GITHUB_ACTIONS:-}" = true ]; then
        printf '::error title=pane_close failed::%s\n' "$stage"
    fi
    exit 1
}
cleanup() {
    tmux_test kill-server >/dev/null 2>&1 || true
    rm -rf "$test_root"
}
trap cleanup EXIT HUP INT TERM

fake_bin=$test_root/bin
mkdir -p "$fake_bin" "$test_root/state"
cat >"$fake_bin/ssh" <<'EOF'
#!/bin/sh
case "$*" in
    *'--format=JobID,User,State,JobName,Elapsed,End,ExitCode,MaxRSS,AllocTRES,Partition,Cluster'*)
        printf '1|offline|FAILED|offline|00:01|2026-08-11T00:00:00+02:00|1:0|1M|cpu=1|test|cispa\n'
        ;;
    *'JobIDRaw,JobID,User,JobName,StdOut,Cluster'*)
        printf '1|1|offline|offline|/tmp/offline-slurm-log|cispa\n'
        ;;
    *'JobID,State,JobName,Elapsed,End,ExitCode'*)
        printf '1|FAILED|offline|00:01|2026-08-11T00:00:00+02:00|1:0|1M|cpu=1|test\n'
        ;;
    *'squeue -h'*) exit 0 ;;
    *) exit 15 ;;
esac
EOF
chmod 755 "$fake_bin/ssh"
config=$test_root/config.json
cat >"$config" <<EOF
{"clusters":[{"name":"cispa","transport":"ssh","user":"offline","sshHost":"offline.invalid","workingDirectory":"$test_root","accounting":true}],"statePath":"$test_root/state/state.json"}
EOF

tmux_test new-session -d -s "$session"
tmux_test set-option -w -t "$session" remain-on-exit on
log_pane=$(tmux_test display-message -p -t "$session" '#{pane_id}')
details_pane=$(tmux_test split-window -d -P -F '#{pane_id}' -t "$log_pane" \
    'while :; do sleep 60; done')
tmux_test set-option -p -t "$details_pane" @slurm_log_detail_parent "$log_pane"
tmux_test respawn-pane -k -t "$log_pane" \
    env PATH="$fake_bin:$tmux_dir:/usr/local/bin:/usr/bin:/bin" \
    HOME="$test_root" \
    SLURM_LOG_LOCAL_USER=offline \
    SLURM_LOG_REMOTE_USER=offline \
    SLURM_LOG_SSH_HOST=offline.invalid \
    SLURM_LOG_CONFIG="$config" \
    SLURM_LOG_STATE="$test_root/state/state.json" \
    "$binary" --pane-follow cispa 1

attempt=0
while :; do
    captured=$(tmux_test capture-pane -p -S -100 -t "$log_pane" 2>/dev/null || true)
    case "$captured" in *'Press Enter to close this pane.'*) break ;; esac
    if [ "$(tmux_test display-message -p -t "$log_pane" '#{pane_dead}')" = 1 ]; then
        fail_stage prompt-process-exited "Pane exited before reaching close prompt"
    fi
    attempt=$((attempt + 1))
    # A loaded shared runner can take more than ten seconds to schedule a
    # freshly spawned PTY process. Keep the wait bounded, but do not mistake
    # runner contention for a product regression.
    if [ "$attempt" -ge 3000 ]; then
        tmux_test list-panes -t "$session" \
            -F 'dead=#{pane_dead} status=#{pane_dead_status} command=#{pane_current_command}' >&2 || true
        fail_stage prompt "Pane never reached close prompt"
    fi
    sleep 0.01
done

tmux_test send-keys -t "$log_pane" Enter
attempt=0
while tmux_test has-session -t "$session" 2>/dev/null; do
    attempt=$((attempt + 1))
    if [ "$attempt" -ge 500 ]; then
        tmux_test list-panes -t "$session" \
            -F 'pane=#{pane_id} parent=#{@slurm_log_detail_parent} dead=#{pane_dead} status=#{pane_dead_status} command=#{pane_current_command}' >&2 || true
        fail_stage teardown "Enter did not close completed pane and its details pane"
    fi
    sleep 0.01
done
printf 'pane_close: ok (real tmux PTY, fake SSH)\n'
