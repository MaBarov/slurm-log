#!/bin/sh
# Offline process coverage for follower paths that must exit normally so LLVM
# can flush coverage profiles. Every scheduler/SSH command is a local fixture.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=${SLURM_LOG_TEST_BINARY:-$project_dir/target/release/slurm-log}
test_root=$(mktemp -d)
case "$test_root" in /tmp/*) ;; *) exit 1 ;; esac
cleanup() {
    case ${follower_pid:-} in ''|*[!0-9]*) ;; *) kill "$follower_pid" >/dev/null 2>&1 || true ;; esac
    rm -rf "$test_root"
}
trap cleanup EXIT HUP INT TERM

fake_bin=$test_root/bin
mkdir -p "$fake_bin" "$test_root/home" "$test_root/state"

# Remote stdout resolution, warning filtering, display, and normal teardown.
cat >"$fake_bin/ssh" <<'EOF'
#!/bin/sh
case "$*" in
    *'scontrol show job 42'*)
        printf 'JobId=42 JobName=remote-train JobState=RUNNING StdOut=/logs/train-%%.log\n'
        ;;
    *'tail -n 7 -F'*)
        printf 'FutureWarning: hidden\n  warnings.warn(old)\nremote line\nValueError: retained\n'
        ;;
    *) exit 23 ;;
esac
EOF
chmod 755 "$fake_bin/ssh"
remote_config=$test_root/remote.json
cat >"$remote_config" <<EOF
{"clusters":[{"name":"remote","transport":"ssh","user":"offline","sshHost":"offline.invalid","workingDirectory":"/work","accounting":false}],"statePath":"$test_root/state/remote.json"}
EOF
env PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" HOME="$test_root/home" \
    SLURM_LOG_CONFIG="$remote_config" \
    "$binary" remote 42 --follow --lines 7 >"$test_root/remote.out"
grep -F 'remote line' "$test_root/remote.out" >/dev/null
grep -F 'ValueError: retained' "$test_root/remote.out" >/dev/null
! grep -F 'FutureWarning' "$test_root/remote.out" >/dev/null

# A completed accounting-only job falls back from scontrol to sacct for its
# stdout path, including a remote tail that exits normally.
cat >"$fake_bin/ssh" <<'EOF'
#!/bin/sh
case "$*" in
    *'scontrol show job 46'*) exit 1 ;;
    *'JobIDRaw,JobID,JobName,StdOut'*)
        printf '46|46|accounted|/logs/accounted.log\n'
        ;;
    *'tail -n 5 -F'*accounted.log*) printf 'accounting fallback line\n' ;;
    *) exit 23 ;;
esac
EOF
chmod 755 "$fake_bin/ssh"
accounting_config=$test_root/accounting.json
cat >"$accounting_config" <<EOF
{"clusters":[{"name":"accounted","transport":"ssh","user":"offline","sshHost":"offline.invalid","workingDirectory":"/work","accounting":true}],"statePath":"$test_root/state/accounting.json"}
EOF
env PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" HOME="$test_root/home" \
    SLURM_LOG_CONFIG="$accounting_config" \
    "$binary" accounted 46 --follow --lines 5 >"$test_root/accounting.out"
grep -F 'accounting fallback line' "$test_root/accounting.out" >/dev/null

# Local registration lag followed by graceful SIGINT child teardown.
cat >"$fake_bin/scontrol" <<EOF
#!/bin/sh
printf 'JobId=43 JobName=local-train JobState=RUNNING StdOut=$test_root/pending.log\n'
EOF
cat >"$fake_bin/squeue" <<'EOF'
#!/bin/sh
printf '43|RUNNING|local-train|00:01|node-a|cpu|now|1|sbatch\n'
EOF
chmod 755 "$fake_bin/scontrol" "$fake_bin/squeue"
local_config=$test_root/local.json
cat >"$local_config" <<EOF
{"clusters":[{"name":"local","transport":"local","user":"offline","workingDirectory":"$test_root","accounting":false}],"statePath":"$test_root/state/local.json"}
EOF
env PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" HOME="$test_root/home" \
    SLURM_LOG_CONFIG="$local_config" \
    "$binary" local 43 --follow >"$test_root/local.out" 2>"$test_root/local.err" &
follower_pid=$!
attempt=0
while ! grep -F 'waiting for its log file' "$test_root/local.out" >/dev/null 2>&1; do
    attempt=$((attempt + 1))
    test "$attempt" -lt 300 || { printf 'local follower did not wait\n' >&2; exit 1; }
    sleep 0.01
done
printf 'local line\n' >"$test_root/pending.log"
attempt=0
while ! grep -F 'local line' "$test_root/local.out" >/dev/null 2>&1; do
    attempt=$((attempt + 1))
    test "$attempt" -lt 300 || { printf 'local follower did not attach\n' >&2; exit 1; }
    sleep 0.01
done
kill -INT "$follower_pid"
wait "$follower_pid"
follower_pid=

# Interactive allocation with no stdout path. Direct PTY exit preserves the
# profile, unlike a process that kills its own tmux pane.
cat >"$fake_bin/ssh" <<'EOF'
#!/bin/sh
case "$*" in
    *'scontrol show job 44'*)
        printf 'JobId=44 JobName=shell JobState=RUNNING BatchFlag=0 Command=bash Partition=gpu NodeList=node-a\n'
        ;;
    *'squeue -h'*)
        printf '44|RUNNING|shell|00:03|node-a|gpu|now|1|bash\n'
        ;;
    *) exit 23 ;;
esac
EOF
chmod 755 "$fake_bin/ssh"
interactive_config=$test_root/interactive.json
cat >"$interactive_config" <<EOF
{"clusters":[{"name":"remote","transport":"ssh","user":"offline","sshHost":"offline.invalid","workingDirectory":"/work","accounting":false}],"statePath":"$test_root/state/interactive.json"}
EOF
(
    sleep 0.25
    printf '\r'
) | env PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" HOME="$test_root/home" \
    SLURM_LOG_CONFIG="$interactive_config" \
    timeout 10 script -qefc "$binary remote 44 --follow" /dev/null \
    >"$test_root/interactive.out"
grep -F 'INTERACTIVE ALLOCATION' "$test_root/interactive.out" >/dev/null
grep -F 'allocation keeps running' "$test_root/interactive.out" >/dev/null
grep -F '"dismissed":{"remote:44":' "$test_root/state/interactive.json" >/dev/null

# A pending ID that fails before publishing stdout renders the final state and
# waits for Enter before suppressing the listing entry.
cat >"$fake_bin/ssh" <<'EOF'
#!/bin/sh
case "$*" in
    *'scontrol show job 45'*) exit 1 ;;
    *'squeue -h'*)
        printf '45|FAILED|early-failure|00:00|Dependency|gpu|Unknown|1|sbatch\n'
        ;;
    *) exit 23 ;;
esac
EOF
chmod 755 "$fake_bin/ssh"
ended_config=$test_root/ended.json
cat >"$ended_config" <<EOF
{"clusters":[{"name":"ended","transport":"ssh","user":"offline","sshHost":"offline.invalid","workingDirectory":"/work","accounting":false}],"statePath":"$test_root/state/ended.json"}
EOF
(
    sleep 0.25
    printf '\r'
) | env PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" HOME="$test_root/home" \
    SLURM_LOG_CONFIG="$ended_config" \
    timeout 10 script -qefc "$binary ended 45 --follow --initial-state PENDING" /dev/null \
    >"$test_root/ended.out"
grep -F 'allocation has ended' "$test_root/ended.out" >/dev/null
grep -F 'FAILED' "$test_root/ended.out" >/dev/null
grep -F '"dismissed":{"ended:45":' "$test_root/state/ended.json" >/dev/null

# The corresponding successful pre-log completion uses the finished alert and
# final monitor frame rather than the failure branch.
cat >"$fake_bin/ssh" <<'EOF'
#!/bin/sh
case "$*" in
    *'scontrol show job 47'*) exit 1 ;;
    *'squeue -h'*)
        printf '47|COMPLETED|short-job|00:01|None|cpu|Unknown|1|sbatch\n'
        ;;
    *) exit 23 ;;
esac
EOF
chmod 755 "$fake_bin/ssh"
finished_config=$test_root/finished.json
cat >"$finished_config" <<EOF
{"clusters":[{"name":"finished","transport":"ssh","user":"offline","sshHost":"offline.invalid","workingDirectory":"/work","accounting":false}],"statePath":"$test_root/state/finished.json"}
EOF
(
    sleep 0.25
    printf '\r'
) | env PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" HOME="$test_root/home" \
    SLURM_LOG_CONFIG="$finished_config" \
    timeout 10 script -qefc "$binary finished 47 --follow --initial-state PENDING" /dev/null \
    >"$test_root/finished.out"
grep -F 'allocation has ended' "$test_root/finished.out" >/dev/null
grep -F 'COMPLETED' "$test_root/finished.out" >/dev/null
grep -F '"dismissed":{"finished:47":' "$test_root/state/finished.json" >/dev/null

printf 'follower_paths: ok (remote/local/interactive/completion; fully offline)\n'
