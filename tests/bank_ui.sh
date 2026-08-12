#!/bin/sh
# Offline PTY regression for bank target switching. It uses local temporary
# files only and never invokes SLURM or SSH.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=$project_dir/target/release/slurm-log
test_root=$(mktemp -d)
session=slurm-log-bank-ui-$$
cleanup() {
    tmux kill-session -t "$session" >/dev/null 2>&1 || true
    tmux list-panes -a -F '#{session_name}|#{@slurm_log_job_id}' 2>/dev/null |
        awk -F '|' '$2 == "987654321" { print $1 }' |
        while IFS= read -r tagged; do tmux kill-session -t "$tagged" >/dev/null 2>&1 || true; done
    tmux kill-server >/dev/null 2>&1 || true
    rm -rf "$test_root"
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$test_root/bank" "$test_root/state" "$test_root/bin" "$test_root/tmux"
chmod 700 "$test_root/tmux"
export TMUX_TMPDIR=$test_root/tmux
tmux new-session -d -s bank-ui-bootstrap sleep 120
printf '#!/bin/sh\n#SBATCH --job-name=local\n' >"$test_root/bank/local_train.sbatch"
printf '#!/bin/sh\n#SBATCH --job-name=remote\n' >"$test_root/bank/remote_train.sbatch"
printf '#!/bin/sh\n#SBATCH --job-name=shared\n' >"$test_root/bank/train.sbatch"
config=$test_root/config.json
cat >"$test_root/bin/sbatch" <<'EOF'
#!/bin/sh
cat >"$BANK_UI_SUBMIT_INPUT"
printf '987654321\n'
EOF
chmod 755 "$test_root/bin/sbatch"
printf '%s\n' "{\"clusters\":[{\"name\":\"local\",\"transport\":\"local\",\"user\":\"offline\",\"sshHost\":\"\",\"workingDirectory\":\"$test_root\",\"accounting\":false},{\"name\":\"remote\",\"transport\":\"ssh\",\"user\":\"offline\",\"sshHost\":\"offline.invalid\",\"workingDirectory\":\"/work\",\"accounting\":true}],\"sbatchBanks\":[{\"path\":\"$test_root/bank\",\"name\":\"Tests\"}],\"statePath\":\"$test_root/state/state.json\"}" >"$config"

tmux new-session -d -x 110 -y 20 -s "$session" \
    env PATH="$test_root/bin:/usr/local/bin:/usr/bin:/bin" \
    BANK_UI_SUBMIT_INPUT="$test_root/submitted.sbatch" \
    SLURM_LOG_CONFIG="$config" "$binary" bank

attempt=0
while :; do
    screen=$(tmux capture-pane -p -t "$session" 2>/dev/null || true)
    case "$screen" in *'SUBMIT TO  [local]  remote'*) break ;; esac
    attempt=$((attempt + 1))
    test "$attempt" -lt 500 || {
        printf 'Initial bank target was not rendered:\n%s\n' "$screen" >&2
        exit 1
    }
    sleep 0.01
done

tmux send-keys -t "$session" Right
attempt=0
while :; do
    screen=$(tmux capture-pane -p -t "$session" 2>/dev/null || true)
    case "$screen" in
        *local_train.sbatch*train.sbatch*)
            case "$screen" in *remote_train.sbatch*) : ;; *) break ;; esac
            ;;
    esac
    attempt=$((attempt + 1))
    test "$attempt" -lt 500 || {
        printf 'Local bank filter was incorrect:\n%s\n' "$screen" >&2
        exit 1
    }
    sleep 0.01
done

tmux send-keys -t "$session" Tab
attempt=0
while :; do
    screen=$(tmux capture-pane -p -t "$session" 2>/dev/null || true)
    case "$screen" in
        *'SUBMIT TO  local  [remote]'*remote_train.sbatch*train.sbatch*)
            case "$screen" in *local_train.sbatch*) : ;; *) break ;; esac
            ;;
    esac
    attempt=$((attempt + 1))
    test "$attempt" -lt 500 || {
        printf 'Tab did not repaint the bank target:\n%s\n' "$screen" >&2
        exit 1
    }
    sleep 0.01
done

tmux send-keys -t "$session" q
attempt=0
while tmux has-session -t "$session" 2>/dev/null; do
    attempt=$((attempt + 1))
    test "$attempt" -lt 500 || exit 1
    sleep 0.01
done

# Submission requires an explicit y, ignores unrelated terminal events, and
# automatically opens the returned job instead of requiring a hidden second o.
tmux new-session -d -x 110 -y 20 -s "$session" \
    env PATH="$test_root/bin:/usr/local/bin:/usr/bin:/bin" \
    BANK_UI_SUBMIT_INPUT="$test_root/submitted.sbatch" \
    SLURM_LOG_CONFIG="$config" "$binary" bank
attempt=0
while :; do
    screen=$(tmux capture-pane -p -t "$session" 2>/dev/null || true)
    case "$screen" in *'SUBMIT TO  [local]  remote'*) break ;; esac
    attempt=$((attempt + 1))
    test "$attempt" -lt 500 || exit 1
    sleep 0.01
done
tmux send-keys -t "$session" Right Down Enter
attempt=0
while :; do
    screen=$(tmux capture-pane -p -t "$session" 2>/dev/null || true)
    case "$screen" in *'Press y to submit and open its pane'*) break ;; esac
    attempt=$((attempt + 1))
    test "$attempt" -lt 500 || { printf 'Submit confirmation missing:\n%s\n' "$screen" >&2; exit 1; }
    sleep 0.01
done
tmux send-keys -t "$session" a
screen=$(tmux capture-pane -p -t "$session")
printf '%s\n' "$screen" | grep -F 'Press y to submit and open its pane' >/dev/null
tmux send-keys -t "$session" y
attempt=0
while test ! -s "$test_root/submitted.sbatch"; do
    attempt=$((attempt + 1))
    test "$attempt" -lt 500 || exit 1
    sleep 0.01
done
cmp "$test_root/bank/local_train.sbatch" "$test_root/submitted.sbatch"
attempt=0
while ! tmux list-panes -a -F '#{@slurm_log_job_id}' | grep -Fx 987654321 >/dev/null; do
    attempt=$((attempt + 1))
    test "$attempt" -lt 500 || { printf 'Submitted job pane was not opened\n' >&2; exit 1; }
    sleep 0.01
done
printf 'bank_ui: ok (cluster filters + reliable submit/open confirmation; fully offline)\n'
