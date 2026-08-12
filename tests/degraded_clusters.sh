#!/bin/sh
# Offline process regression for ambiguous job IDs and partial cluster outages.
# Every scheduler and SSH command is a local fixture.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=${SLURM_LOG_TEST_BINARY:-$project_dir/target/release/slurm-log}
test_root=$(mktemp -d)
case "$test_root" in /tmp/*) ;; *) exit 1 ;; esac
fake_bin=$test_root/bin
normal_config=$test_root/normal.json
degraded_config=$test_root/degraded.json
mkdir -p "$fake_bin" "$test_root/normal-state" "$test_root/degraded-state"

stop_daemon() {
    config=$1
    env PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" HOME="$test_root" \
        SLURM_LOG_CONFIG="$config" "$binary" daemon stop >/dev/null 2>&1 || true
}
cleanup() {
    stop_daemon "$normal_config"
    stop_daemon "$degraded_config"
    rm -rf "$test_root"
}
trap cleanup EXIT HUP INT TERM

cat >"$fake_bin/squeue" <<'EOF'
#!/bin/sh
printf '777|RUNNING|alpha-job|00:01|alpha-node|cpu|2026-08-12T10:00:00|1000|train.sbatch\n'
EOF
cat >"$fake_bin/ssh" <<'EOF'
#!/bin/sh
case "${OFFLINE_CLUSTER_MODE:-normal}:$*" in
    normal:*'squeue -h'*)
        printf '777|RUNNING|beta-job|00:02|beta-node|gpu|2026-08-12T10:00:00|2000|remote.sbatch\n'
        ;;
    normal:*'sacct -X -S'*) : ;;
    degraded:*)
        printf 'simulated SSH outage\n' >&2
        exit 23
        ;;
    *)
        printf 'unexpected fake SSH request: %s\n' "$*" >&2
        exit 24
        ;;
esac
EOF
cat >"$fake_bin/sacct" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod 755 "$fake_bin/squeue" "$fake_bin/ssh" "$fake_bin/sacct"

cat >"$normal_config" <<EOF
{
  "clusters": [
    {"name":"alpha","transport":"local","user":"offline","workingDirectory":"$test_root","accounting":false},
    {"name":"beta","transport":"ssh","user":"offline","sshHost":"beta.invalid","workingDirectory":"/offline","accounting":true}
  ],
  "statePath":"$test_root/normal-state/state.json"
}
EOF
cat >"$degraded_config" <<EOF
{
  "clusters": [
    {"name":"alpha","transport":"local","user":"offline","workingDirectory":"$test_root","accounting":false},
    {"name":"beta","transport":"ssh","user":"offline","sshHost":"beta.invalid","workingDirectory":"/offline","accounting":true}
  ],
  "statePath":"$test_root/degraded-state/state.json"
}
EOF

common_env="PATH=$fake_bin:/usr/local/bin:/usr/bin:/bin HOME=$test_root"

# An unqualified ID shared by two clusters must never silently pick one. A
# missing ID should tell the user how to disambiguate instead of querying an
# arbitrary scheduler.
if env $common_env OFFLINE_CLUSTER_MODE=normal SLURM_LOG_CONFIG="$normal_config" \
    "$binary" details 777 >"$test_root/ambiguous.out" 2>"$test_root/ambiguous.err"; then
    printf 'ambiguous details unexpectedly succeeded\n' >&2
    exit 1
fi
grep -F 'job 777 exists on multiple clusters' "$test_root/ambiguous.err" >/dev/null
grep -F 'specify --cluster NAME (alpha, beta)' "$test_root/ambiguous.err" >/dev/null

if env $common_env OFFLINE_CLUSTER_MODE=normal SLURM_LOG_CONFIG="$normal_config" \
    "$binary" details 999 >"$test_root/missing.out" 2>"$test_root/missing.err"; then
    printf 'missing details unexpectedly succeeded\n' >&2
    exit 1
fi
grep -F 'job 999 is not in the live/recent cache' "$test_root/missing.err" >/dev/null
grep -F 'specify --cluster NAME (alpha, beta)' "$test_root/missing.err" >/dev/null
stop_daemon "$normal_config"

# One failed cluster must not erase healthy-cluster jobs. Both the live and
# archive commands surface the outage, while archive also explains why an
# accounting-disabled cluster cannot list completed jobs.
env $common_env OFFLINE_CLUSTER_MODE=degraded SLURM_LOG_CONFIG="$degraded_config" \
    "$binary" all --cluster all >"$test_root/live.out" 2>"$test_root/live.err"
grep -F alpha-job "$test_root/live.out" >/dev/null
grep -F 'warning: beta:' "$test_root/live.err" >/dev/null
grep -F 'simulated SSH outage' "$test_root/live.err" >/dev/null

env $common_env OFFLINE_CLUSTER_MODE=degraded SLURM_LOG_CONFIG="$degraded_config" \
    "$binary" archive --cluster all >"$test_root/archive.out" 2>"$test_root/archive.err"
grep -F alpha-job "$test_root/archive.out" >/dev/null
grep -F 'alpha: completed jobs unavailable because sacct/accounting is disabled' \
    "$test_root/archive.err" >/dev/null
grep -F 'warning: beta accounting:' "$test_root/archive.err" >/dev/null

printf 'degraded_clusters: ok (ambiguity + partial outage + accounting warning; fully offline)\n'
