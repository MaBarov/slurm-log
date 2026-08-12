#!/bin/sh
# Offline process-level regression for daemon lifecycle, canonical snapshots,
# cache separation, query coalescing, scheduler failures, and auto-start.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=${SLURM_LOG_TEST_BINARY:-$project_dir/target/release/slurm-log}
test_root=$(mktemp -d)
case "$test_root" in /tmp/*) ;; *) exit 1 ;; esac
fake_bin=$test_root/bin
home_dir=$test_root/home
calls=$test_root/calls
mkdir -p "$fake_bin" "$home_dir"

cleanup() {
    for config in "$test_root"/config-*.json; do
        test -f "$config" || continue
        env PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" HOME="$home_dir" \
            SLURM_LOG_CONFIG="$config" "$binary" daemon stop >/dev/null 2>&1 || true
    done
    rm -rf "$test_root"
}
trap cleanup EXIT HUP INT TERM

cat >"$fake_bin/squeue" <<'EOF'
#!/bin/sh
printf 'squeue %s\n' "$*" >>"$DAEMON_CALL_LOG"
if test "${DAEMON_SCHEDULER_FAIL:-0}" = 1; then
    printf 'offline squeue failure\n' >&2
    exit 71
fi
printf '101|RUNNING|daemon-running|00:01|node-a|cpu|2026-08-12T10:00:00|100|run.sbatch\n'
printf '102|PENDING|daemon-pending|00:00|Resources|gpu|Unknown|20|wait.sbatch\n'
EOF
cat >"$fake_bin/sacct" <<'EOF'
#!/bin/sh
printf 'sacct %s\n' "$*" >>"$DAEMON_CALL_LOG"
if test "${DAEMON_SCHEDULER_FAIL:-0}" = 1; then
    printf 'offline sacct failure\n' >&2
    exit 72
fi
printf '301|FAILED|daemon-failed|00:03|%s|1:0|1G|cpu=2,mem=2G|cpu\n' "$DAEMON_NOW"
printf '302|COMPLETED|daemon-complete|00:04|%s|0:0|1G|cpu=2,mem=2G|cpu\n' "$DAEMON_NOW"
EOF
chmod 755 "$fake_bin/squeue" "$fake_bin/sacct"

now=$(date --iso-8601=seconds)
export PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin"
export HOME=$home_dir
export XDG_CONFIG_HOME=$home_dir/config
export XDG_STATE_HOME=$home_dir/state
export DAEMON_CALL_LOG=$calls
export DAEMON_NOW=$now
export SLURM_LOG_ARCHIVE_DAYS=5

make_config() {
    name=$1
    state_dir=$test_root/$name
    config=$test_root/config-$name.json
    mkdir -p "$state_dir"
    cat >"$config" <<EOF
{"clusters":[{"name":"alpha","transport":"local","user":"offline","workingDirectory":"$test_root","accounting":true}],"statePath":"$state_dir/state.json"}
EOF
    printf '%s\n' "$config"
}

wait_running() {
    config=$1
    attempt=0
    until SLURM_LOG_CONFIG="$config" "$binary" daemon status >/dev/null 2>&1; do
        attempt=$((attempt + 1))
        test "$attempt" -lt 200 || { printf 'daemon did not become ready\n' >&2; exit 1; }
        sleep 0.01
    done
}

# Explicit lifecycle is idempotent and the IPC endpoint is owner-only.
config=$(make_config lifecycle)
if SLURM_LOG_CONFIG="$config" "$binary" daemon status >/dev/null 2>&1; then
    printf 'stopped daemon incorrectly reported running\n' >&2
    exit 1
fi
SLURM_LOG_CONFIG="$config" "$binary" daemon start | grep -F 'daemon started' >/dev/null
wait_running "$config"
SLURM_LOG_CONFIG="$config" "$binary" daemon start | grep -F 'daemon started' >/dev/null
socket=$test_root/lifecycle/daemon.sock
test -S "$socket"
test "$(stat -c %a "$socket")" = 600

# One canonical live snapshot serves every filter without more scheduler RPCs.
: >"$calls"
SLURM_LOG_CONFIG="$config" "$binary" json --cluster alpha >"$test_root/all.json"
grep -F daemon-running "$test_root/all.json" >/dev/null
grep -F daemon-failed "$test_root/all.json" >/dev/null
before=$(wc -l <"$calls")
SLURM_LOG_CONFIG="$config" "$binary" running --cluster alpha >"$test_root/running"
SLURM_LOG_CONFIG="$config" "$binary" failed --cluster alpha >"$test_root/failed"
test "$(wc -l <"$calls")" -eq "$before"
grep -F daemon-running "$test_root/running" >/dev/null
! grep -F daemon-failed "$test_root/running" >/dev/null
grep -F daemon-failed "$test_root/failed" >/dev/null

# Archive snapshots are separate, while the still-fresh queue disk cache is
# reused. The live query issued one squeue + one sacct; archive adds one sacct.
SLURM_LOG_CONFIG="$config" "$binary" archive --cluster alpha >"$test_root/archive"
grep -F daemon-complete "$test_root/archive" >/dev/null
test "$(grep -c '^squeue ' "$calls")" -eq 1
test "$(grep -c '^sacct ' "$calls")" -eq 2

SLURM_LOG_CONFIG="$config" "$binary" daemon stop | grep -F 'daemon stopped' >/dev/null
attempt=0
while test -e "$socket"; do
    attempt=$((attempt + 1)); test "$attempt" -lt 200; sleep 0.01
done
if SLURM_LOG_CONFIG="$config" "$binary" daemon status >/dev/null 2>&1; then exit 1; fi

# Concurrent cold clients are serialized behind one daemon snapshot/query-lock
# fill. This is a small local contention check, never a scheduler stress test.
config_parallel=$(make_config parallel)
SLURM_LOG_CONFIG="$config_parallel" "$binary" daemon start >/dev/null
wait_running "$config_parallel"
: >"$calls"
pids=
index=1
while test "$index" -le 8; do
    SLURM_LOG_CONFIG="$config_parallel" "$binary" json --cluster alpha \
        >"$test_root/parallel-$index.json" &
    pids="$pids $!"
    index=$((index + 1))
done
for pid in $pids; do wait "$pid"; done
index=1
while test "$index" -le 8; do
    grep -F daemon-running "$test_root/parallel-$index.json" >/dev/null
    index=$((index + 1))
done
test "$(grep -c '^squeue ' "$calls")" -eq 1
test "$(grep -c '^sacct ' "$calls")" -eq 1
SLURM_LOG_CONFIG="$config_parallel" "$binary" daemon stop >/dev/null

# A query auto-starts a stopped daemon. Per-source scheduler failures degrade
# to visible warnings and do not crash the request server.
config_auto=$(make_config auto)
: >"$calls"
SLURM_LOG_CONFIG="$config_auto" "$binary" json --cluster alpha >"$test_root/auto.json"
grep -F daemon-running "$test_root/auto.json" >/dev/null
wait_running "$config_auto"
SLURM_LOG_CONFIG="$config_auto" "$binary" daemon stop >/dev/null

config_fail=$(make_config failure)
DAEMON_SCHEDULER_FAIL=1 SLURM_LOG_CONFIG="$config_fail" "$binary" daemon start >/dev/null
wait_running "$config_fail"
DAEMON_SCHEDULER_FAIL=1 SLURM_LOG_CONFIG="$config_fail" \
    "$binary" all --cluster alpha >"$test_root/failure.out" 2>"$test_root/failure.err"
grep -F 'warning: alpha: offline squeue failure' "$test_root/failure.err" >/dev/null
grep -F 'warning: alpha accounting:' "$test_root/failure.err" >/dev/null
SLURM_LOG_CONFIG="$config_fail" "$binary" daemon status >/dev/null
SLURM_LOG_CONFIG="$config_fail" "$binary" daemon stop >/dev/null

printf 'daemon_integration: ok (lifecycle, private IPC, caches, concurrency, failure; fully offline)\n'
