#!/bin/sh
# Hermetic hostile scheduler/SSH integration test. Every external cluster
# command resolves to a temporary fake; no network or real SLURM access occurs.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=${SLURM_LOG_TEST_BINARY:-$project_dir/target/release/slurm-log}
test_root=$(mktemp -d)
case "$test_root" in /tmp/*) ;; *) printf 'Unsafe temp path\n' >&2; exit 1 ;; esac
cleanup() {
    "$binary" daemon stop >/dev/null 2>&1 || true
    if [ "${SLURM_LOG_PRESERVE_FIXTURE:-0}" = 1 ]; then
        printf 'Preserved hostile fixture: %s\n' "$test_root" >&2
    else
        rm -rf "$test_root"
    fi
}
trap cleanup EXIT HUP INT TERM

fake_bin=$test_root/fake-bin
home_dir=$test_root/home
call_log=$test_root/calls
marker=$test_root/injected
mkdir -p "$fake_bin" "$home_dir/state"

cat >"$fake_bin/squeue" <<'EOF'
#!/bin/sh
printf '%s\n' "$0 $*" >>"$OFFLINE_CALL_LOG"
printf '101|RUNNING|safe-local|00:01|node|gpu|2026-08-11T12:00:00|1000||offline-local\n'
printf '101.batch|RUNNING|step-must-be-rejected|00:01|node\n'
printf 'bad-id|RUNNING|malformed|00:01|node\n'
printf '102|PENDING|$(touch %s)|00:00|DependencyNeverSatisfied\n' "$OFFLINE_MARKER"
EOF

cat >"$fake_bin/ssh" <<'EOF'
#!/bin/sh
printf '%s\n' "$0 $*" >>"$OFFLINE_CALL_LOG"
case "$*" in
    *'squeue -h'*) printf '201|RUNNING|safe-remote|00:02|gpu01|gpu|2026-08-11T12:00:00|2000||offline-remote\n' ;;
    *'--format=JobID,User,State,JobName,Elapsed,End,ExitCode,MaxRSS,AllocTRES,Partition,Cluster'*)
        printf '202|offline-remote|FAILED|remote-failure|00:03|2026-08-11T00:04:00|1:0|4G|gres/gpu=1|gpu|cispa\n'
        ;;
    *' -j 202 '*)
        printf '202|remote-failure|FAILED|OutOfMemory|gpu|acct|normal|2026-08-11T00:00:00|2026-08-11T00:01:00|2026-08-11T00:04:00|00:03:00|180|01:00:00|1|8|8|8|16Gc|2G|1G|cpu=8,mem=64G,gres/gpu:a100=1|cpu=8,mem=64G,gres/gpu:a100=1|00:01:30|1440|1:0|gpu01|||offline-remote|cispa\n'
        ;;
    *' -X -S now-1hour '*)
        printf '202|FAILED|remote-failure|00:03|2026-08-11T00:00:00+02:00|1:0|4G|gres/gpu=1|gpu|offline-remote|cispa\n'
        printf 'not-a-job|FAILED|rejected|00:03|2026-08-11T00:00:00+02:00\n'
        ;;
    *) printf 'unexpected fake ssh request\n' >&2; exit 23 ;;
esac
EOF

cat >"$fake_bin/scontrol" <<'EOF'
#!/bin/sh
printf '%s\n' "$0 $*" >>"$OFFLINE_CALL_LOG"
printf 'JobId=101 JobName=safe StdOut=/tmp/safe.log JobState=RUNNING\n'
EOF

cat >"$fake_bin/tmux" <<'EOF'
#!/bin/sh
printf '%s\n' "$0 $*" >>"$OFFLINE_CALL_LOG"
exit 99
EOF

chmod 755 "$fake_bin/ssh" "$fake_bin/squeue" "$fake_bin/scontrol" "$fake_bin/tmux"
export PATH=$fake_bin:/usr/local/bin:/usr/bin:/bin
export HOME=$home_dir
export XDG_CONFIG_HOME=$home_dir/config
export XDG_STATE_HOME=$home_dir/state
export SLURM_LOG_LOCAL_USER=offline-local
export SLURM_LOG_REMOTE_USER=offline-remote
export SLURM_LOG_SSH_HOST=offline.invalid
export SLURM_LOG_STATE=$home_dir/state/slurm-log/state.json
export OFFLINE_CALL_LOG=$call_log
export OFFLINE_MARKER=$marker
mkdir -p "$XDG_CONFIG_HOME/slurm-log"
cat >"$XDG_CONFIG_HOME/slurm-log/config.json" <<EOF
{
  "clusters": [
    {"name":"local","transport":"local","user":"offline-local","workingDirectory":"$home_dir","accounting":false},
    {"name":"cispa","transport":"ssh","user":"offline-remote","sshHost":"offline.invalid","workingDirectory":"$home_dir","accounting":true}
  ],
  "statePath":"$SLURM_LOG_STATE"
}
EOF

output=$test_root/jobs.json
"$binary" json --cluster both >"$output"
grep -q '"id": "101"' "$output"
grep -q '"id": "201"' "$output"
grep -q '"id": "202"' "$output" || {
    printf 'Accounting row missing from mocked output:\n' >&2
    cat "$output" >&2
    printf 'Fake command trace:\n' >&2
    cat "$call_log" >&2
    exit 1
}
grep -q '"exit_code": "1:0"' "$output" || {
    printf 'Extended accounting fields missing from mocked output:\n' >&2
    cat "$output" >&2
    exit 1
}
grep -q '"alloc_tres": "gres/gpu=1"' "$output"
! grep -q '101.batch\|not-a-job\|bad-id' "$output"
test ! -e "$marker"
test "$(grep -c '/ssh ' "$call_log")" -eq 2
test "$(grep -c '/squeue ' "$call_log")" -eq 1
! grep -q -- '-oProxyCommand\|ProxyCommand=' "$call_log"

# The visual details command prints one stable snapshot when redirected. Its
# accounting request remains inside the fake SSH boundary.
details_output=$test_root/details.txt
"$binary" details 202 --cluster cispa >"$details_output"
grep -q 'Allocation: 1 nodes, 8 CPUs, 1 GPUs' "$details_output"
grep -q 'Utilization: CPU 6.2%, memory 3.1%' "$details_output"
grep -q 'sacct .* -j 202' "$call_log"

# Invalid dimensions and option-like hosts must fail before any cluster call.
: >"$call_log"
if "$binary" json --cluster ../../escape >/dev/null 2>&1; then exit 1; fi
if "$binary" json --ssh-host -oProxyCommand=evil >/dev/null 2>&1; then exit 1; fi
test ! -s "$call_log"

printf 'offline_hostile: ok (fake commands only; no network or SLURM)\n'
