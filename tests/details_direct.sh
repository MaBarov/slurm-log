#!/bin/sh
# Fully offline, normally exiting PTY coverage for the direct details UI.
# Scheduler tools are local fixtures and daemon sockets live below /tmp.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=${SLURM_LOG_TEST_BINARY:-$project_dir/target/release/slurm-log}
test_root=$(mktemp -d)
case "$test_root" in /tmp/*) ;; *) exit 1 ;; esac
fake_bin=$test_root/bin
mkdir -p "$fake_bin" "$test_root/home"
cleanup() {
    for config in "$test_root"/*.json; do
        test -f "$config" || continue
        env PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" HOME="$test_root/home" \
            SLURM_LOG_CONFIG="$config" "$binary" daemon stop >/dev/null 2>&1 || true
    done
    rm -rf "$test_root"
}
trap cleanup EXIT HUP INT TERM

cat >"$fake_bin/squeue" <<'EOF'
#!/bin/sh
case "${DETAIL_SCENARIO:-}" in
    live) printf '77|offline|RUNNING|live-job|00:02:00|node-a|gpu|start|1|sbatch\n' ;;
    *) : ;;
esac
EOF
cat >"$fake_bin/scontrol" <<'EOF'
#!/bin/sh
test "${DETAIL_SCENARIO:-}" = live || exit 31
printf 'JobId=77 UserId=offline(1000) JobName=live-job JobState=RUNNING Reason=None RunTime=00:02:00 TimeLimit=01:00:00 NumNodes=1 NumCPUs=8 Partition=gpu NodeList=node-a Account=lab QOS=normal ReqTRES=cpu=8,mem=16G,gres/gpu:a100=1 AllocTRES=cpu=8,mem=16G,gres/gpu:a100=1 ExitCode=0:0\n'
EOF
cat >"$fake_bin/sstat" <<'EOF'
#!/bin/sh
printf '77.batch|8|cpu=8,mem=16G|00:01:00|4G|2G|gres/gpuutil=75|gres/gpumem=8G\n'
EOF
cat >"$fake_bin/sacct" <<'EOF'
#!/bin/sh
test "${DETAIL_SCENARIO:-}" = terminal || exit 32
case "$*" in
    *'JobID,User,State,JobName,Elapsed,End,ExitCode,MaxRSS,AllocTRES,Partition,Cluster'*)
        printf '88|offline|COMPLETED|done-job|00:10:00|end|0:0|8G|cpu=8,mem=16G,gres/gpu:a100=1|gpu|terminal\n'
        ;;
    *)
        printf '88|done-job|COMPLETED|None|gpu|lab|normal|submit|start|end|00:10:00|600|01:00:00|1|8|8|8|16G|8G|4G|cpu=8,mem=16G,gres/gpu:a100=1|cpu=8,mem=16G,gres/gpu:a100=1|01:00:00|4800|0:0|node-b|||offline|terminal\n'
        ;;
esac
EOF
chmod 755 "$fake_bin"/*

make_config() {
    name=$1
    accounting=$2
    state_dir=$test_root/state-$name
    mkdir -p "$state_dir"
    cat >"$test_root/$name.json" <<EOF
{"clusters":[{"name":"$name","transport":"local","user":"offline","workingDirectory":"$test_root","accounting":$accounting}],"statePath":"$state_dir/state.json"}
EOF
}
make_config live false
make_config terminal true
make_config broken false

# Live full view: resize, pause/resume, force refresh, and Enter close.
(
    sleep 0.35
    printf ' '
    sleep 0.1
    printf ' '
    sleep 0.1
    printf 'r'
    sleep 0.25
    printf '\r'
) | env PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" HOME="$test_root/home" \
    DETAIL_SCENARIO=live SLURM_LOG_CONFIG="$test_root/live.json" \
    timeout 12 script -qefc "$binary details 77 --cluster live" /dev/null \
    >"$test_root/live.out"
grep -F 'live-job' "$test_root/live.out" >/dev/null
grep -F 'CPU trend' "$test_root/live.out" >/dev/null
grep -E 'refreshing|refreshed|refresh queued' "$test_root/live.out" >/dev/null
env PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" HOME="$test_root/home" \
    SLURM_LOG_CONFIG="$test_root/live.json" "$binary" daemon stop >/dev/null 2>&1 || true

# Terminal compact view: refresh is explicitly a final snapshot.
(
    sleep 0.35
    printf 'r'
    sleep 0.1
    printf '\r'
) | env PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" HOME="$test_root/home" \
    DETAIL_SCENARIO=terminal SLURM_LOG_DETAILS_COMPACT=1 \
    SLURM_LOG_CONFIG="$test_root/terminal.json" \
    timeout 12 script -qefc "$binary details 88 --cluster terminal" /dev/null \
    >"$test_root/terminal.out"
grep -F 'done-job' "$test_root/terminal.out" >/dev/null
grep -F 'final snapshot' "$test_root/terminal.out" >/dev/null
env PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" HOME="$test_root/home" \
    SLURM_LOG_CONFIG="$test_root/terminal.json" "$binary" daemon stop >/dev/null 2>&1 || true

# Retryable error view: initial failure plus a manually requested failure keep
# the UI responsive and close normally.
(
    sleep 0.35
    printf 'r'
    sleep 0.2
    printf '\r'
) | env PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" HOME="$test_root/home" \
    DETAIL_SCENARIO=broken SLURM_LOG_CONFIG="$test_root/broken.json" \
    timeout 12 script -qefc "$binary details 99 --cluster broken" /dev/null \
    >"$test_root/broken.out"
grep -F 'UNAVAILABLE' "$test_root/broken.out" >/dev/null
grep -E 'refresh failed|stale:' "$test_root/broken.out" >/dev/null
env PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" HOME="$test_root/home" \
    SLURM_LOG_CONFIG="$test_root/broken.json" "$binary" daemon stop >/dev/null 2>&1 || true

printf 'details_direct: ok (live/terminal/error controls; fully offline)\n'
