#!/bin/sh
# Hermetic MCP owner-transition regression: fake commands only.
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=${SLURM_LOG_TEST_BINARY:-$project_dir/target/release/slurm-log}
test_root=$(mktemp -d)
case "$test_root" in /tmp/*) ;; *) exit 1 ;; esac
fake_bin=$test_root/bin
state_dir=$test_root/state
work=$test_root/work
mkdir -p "$fake_bin" "$state_dir" "$work"

cleanup() {
    SLURM_LOG_CONFIG="$test_root/config.json" "$binary" daemon stop >/dev/null 2>&1 || true
    rm -rf "$test_root"
}
trap cleanup EXIT HUP INT TERM

cat >"$fake_bin/squeue" <<'EOF'
#!/bin/sh
# Exact authorization requests use the explicit %u user column.  A regular
# list keeps the legacy nine-field shape for unrelated rendering requests.
case " $* " in
  *" -j "*)
    if test "${MCP_OWNER_MODE:-foreign}" = owner; then
        printf '7|owner|RUNNING|owned|00:01|node|cpu|start|1|job.sbatch\n'
    else
        printf '7|other|RUNNING|foreign|00:01|node|cpu|start|1|job.sbatch\n'
    fi
    ;;
  *) printf '7|RUNNING|owned|00:01|node|cpu|start|1|job.sbatch|owner\n' ;;
esac
EOF
cat >"$fake_bin/scontrol" <<'EOF'
#!/bin/sh
printf 'JobId=7 UserId=other(2000) JobName=FOREIGN_CONTROL_SECRET JobState=RUNNING StdOut=%s/foreign.log Dependency=after:99\n' "$MCP_OWNER_WORK"
EOF
cat >"$fake_bin/sstat" <<'EOF'
#!/bin/sh
printf '7.batch|1|cpu=1|00:01|1K|1K|||\n'
EOF
chmod 755 "$fake_bin/squeue" "$fake_bin/scontrol" "$fake_bin/sstat"

printf 'FOREIGN_LOG_SECRET\n' >"$work/foreign.log"
cat >"$test_root/config.json" <<EOF
{
  "clusters": [
    {"name":"local","transport":"local","user":"owner","workingDirectory":"$work","accounting":false}
  ],
  "statePath":"$state_dir/state.json"
}
EOF
chmod 600 "$test_root/config.json"
export PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin"
export HOME="$test_root/home"
export MCP_OWNER_WORK=$work
export SLURM_LOG_CONFIG="$test_root/config.json"

requests() {
    printf '%s\n' \
        '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"offline","version":"1"}}}' \
        '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
        '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"slurm_inspect_job","arguments":{"cluster":"local","job_id":"7"}}}' \
        '{"jsonrpc":"2.0","id":3,"method":"resources/read","params":{"uri":"slurm-log://jobs/local/7"}}' \
        '{"jsonrpc":"2.0","id":4,"method":"resources/read","params":{"uri":"slurm-log://jobs/local/7/details"}}' \
        '{"jsonrpc":"2.0","id":5,"method":"resources/read","params":{"uri":"slurm-log://jobs/local/7/log"}}' \
        '{"jsonrpc":"2.0","id":6,"method":"resources/subscribe","params":{"uri":"slurm-log://jobs/local/7/details"}}'
}

# An ID that now belongs to somebody else must be rejected before inspect,
# concrete resources, and subscriptions reach their metadata/log paths.
requests | MCP_OWNER_MODE=foreign "$binary" mcp >"$test_root/foreign.out"
test "$(grep -c 'owned by the configured' "$test_root/foreign.out")" -ge 5
! grep -F 'FOREIGN_CONTROL_SECRET' "$test_root/foreign.out"
! grep -F 'FOREIGN_LOG_SECRET' "$test_root/foreign.out"

# Even when the fresh queue grant is initially ours, every subsequent scontrol
# record is identity-checked.  The foreign controller reply is never exposed.
requests | MCP_OWNER_MODE=owner "$binary" mcp >"$test_root/followup.out"
! grep -F 'FOREIGN_CONTROL_SECRET' "$test_root/followup.out"
! grep -F 'FOREIGN_LOG_SECRET' "$test_root/followup.out"
grep -F '"id":2' "$test_root/followup.out" >/dev/null
grep -F '"id":4' "$test_root/followup.out" >/dev/null

printf 'mcp_owner_isolation: ok (fake scheduler only)\n'
