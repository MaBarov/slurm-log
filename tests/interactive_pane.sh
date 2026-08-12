#!/bin/sh
# Offline regression for interactive allocations that have no Slurm stdout.
# Fake SSH/scheduler output only; this never contacts a cluster.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=$project_dir/target/release/slurm-log
test_root=$(mktemp -d)
session=slurm-log-interactive-test-$$
cleanup() {
    tmux kill-session -t "$session" >/dev/null 2>&1 || true
    rm -rf "$test_root"
}
trap cleanup EXIT HUP INT TERM

fake_bin=$test_root/bin
calls=$test_root/calls
mkdir -p "$fake_bin" "$test_root/state"
cat >"$fake_bin/ssh" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"$INTERACTIVE_CALL_LOG"
case "$*" in
    *'scontrol show job 9'*)
        printf 'JobId=9 JobName=interactive-shell JobState=RUNNING BatchFlag=0 NodeList=gpu-01 Command=bash WorkDir=/tmp\n'
        ;;
    *'squeue -h'*)
        printf '9|RUNNING|interactive-shell|00:42|gpu-01|gpu|2026-08-12T10:00:00|1\n'
        ;;
    *) exit 23 ;;
esac
EOF
chmod 755 "$fake_bin/ssh"
config=$test_root/config.json
cat >"$config" <<EOF
{"clusters":[{"name":"cispa","transport":"ssh","user":"offline","sshHost":"offline.invalid","workingDirectory":"$test_root","accounting":false}],"statePath":"$test_root/state/state.json"}
EOF

tmux new-session -d -s "$session"
tmux set-option -w -t "$session" remain-on-exit on
pane=$(tmux display-message -p -t "$session" '#{pane_id}')
tmux respawn-pane -k -t "$pane" \
    env PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" \
    HOME="$test_root" \
    INTERACTIVE_CALL_LOG="$calls" \
    SLURM_LOG_CONFIG="$config" \
    SLURM_LOG_STATE="$test_root/state/state.json" \
    "$binary" --pane-follow cispa 9

attempt=0
while :; do
    captured=$(tmux capture-pane -p -S -100 -t "$pane" 2>/dev/null || true)
    case "$captured" in *'INTERACTIVE ALLOCATION'*'BatchFlag=0'*'allocation keeps running'*) break ;; esac
    attempt=$((attempt + 1))
    if [ "$attempt" -ge 500 ]; then
        printf 'Interactive monitor did not render:\n%s\n' "$captured" >&2
        exit 1
    fi
    sleep 0.01
done

test "$(tmux display-message -p -t "$pane" '#{pane_dead}')" = 0
! grep -q 'tail ' "$calls"
tmux send-keys -t "$pane" Enter
attempt=0
while tmux has-session -t "$session" 2>/dev/null; do
    attempt=$((attempt + 1))
    test "$attempt" -lt 500 || {
        printf 'Enter did not close the interactive monitor\n' >&2
        exit 1
    }
    sleep 0.01
done
grep -F '"dismissed":{"cispa:9":' "$test_root/state/state.json" >/dev/null
printf 'interactive_pane: ok (live monitor, safe Enter close + list suppression; fully offline)\n'
