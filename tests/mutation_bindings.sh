#!/bin/sh
# Focused offline regression for controller-bound mutations and exact array
# cancellation. Every scheduler and SSH executable below is a local fixture.
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=${SLURM_LOG_TEST_BINARY:-$project_dir/target/release/slurm-log}
test_root=$(mktemp -d)
case "$test_root" in /tmp/*) ;; *) exit 1 ;; esac
fake_bin=$test_root/bin
calls=$test_root/calls

cleanup() { rm -rf "$test_root"; }
trap cleanup EXIT HUP INT TERM

mkdir -p "$fake_bin" "$test_root/bank" "$test_root/work" "$test_root/state"
cat >"$test_root/bank/remote.sbatch" <<'EOF'
#!/bin/sh
#SBATCH --job-name=remote-bound
EOF
cat >"$test_root/bank/routed-away.sbatch" <<'EOF'
#!/bin/sh
#SBATCH --job-name=routed-away
#SBATCH --clusters=other-controller
EOF
cat >"$test_root/bank/short-routed-away.sbatch" <<'EOF'
#!/bin/sh
#SBATCH --job-name=short-routed-away
#SBATCH -Mother-controller
EOF

cat >"$fake_bin/squeue" <<'EOF'
#!/bin/sh
printf 'local-squeue %s\n' "$*" >>"$MUTATION_CALLS"
case " $* " in
  *' -j 700 '*) printf '700|RUNNING|array-train|00:01|node|cpu|now|1|array.sbatch|offline\n' ;;
  *' -j 700_3 '*) printf '700_3|RUNNING|array-train|00:01|node|cpu|now|1|array.sbatch|offline\n' ;;
  *) exit 71 ;;
esac
EOF
cat >"$fake_bin/scontrol" <<'EOF'
#!/bin/sh
printf 'local-scontrol %s\n' "$*" >>"$MUTATION_CALLS"
case " $* " in
  *' show job -o 700 '*)
    printf 'JobId=700 UserId=offline(1000) JobName=array-train JobState=RUNNING ArrayJobId=700 ArrayTaskId=0-9\n'
    ;;
  *' show job -o 700_3 '*)
    printf 'JobId=700_3 UserId=offline(1000) JobName=array-train JobState=RUNNING ArrayJobId=700 ArrayTaskId=3\n'
    ;;
  *) exit 72 ;;
esac
EOF
cat >"$fake_bin/scancel" <<'EOF'
#!/bin/sh
printf 'local-scancel %s\n' "$*" >>"$MUTATION_CALLS"
EOF
cat >"$fake_bin/ssh" <<'EOF'
#!/bin/sh
for remote do :; done
printf '%s\n' "$remote" >>"$MUTATION_SSH_COMMANDS"
case "$remote" in
  *'sbatch '*) printf '%s\n' "${MUTATION_SSH_RESPONSE:-9001;controller-a}" ;;
  *'squeue '*) printf '9001|RUNNING|remote-cancel|00:01|node|cpu|now|1|remote.sbatch|offline\n' ;;
  *'scontrol '*) printf 'JobId=9001 UserId=offline(1000) JobName=remote-cancel JobState=RUNNING\n' ;;
  *'scancel '*) : ;;
  *) exit 73 ;;
esac
EOF
chmod 755 "$fake_bin/squeue" "$fake_bin/scontrol" "$fake_bin/scancel" "$fake_bin/ssh"

cat >"$test_root/config.json" <<EOF
{
  "clusters": [
    {"name":"local-label","controller":"local-controller","transport":"local","user":"offline","workingDirectory":"$test_root/work","accounting":false},
    {"name":"remote-label","controller":"controller-a","transport":"ssh","user":"offline","sshHost":"offline.invalid","workingDirectory":"/trusted/work","accounting":false}
  ],
  "sbatchBanks": [{"path":"$test_root/bank","name":"Bank"}],
  "statePath":"$test_root/state/state.json"
}
EOF
chmod 600 "$test_root/config.json"

export PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin"
export SLURM_LOG_CONFIG="$test_root/config.json"
export MUTATION_CALLS="$calls"
export MUTATION_SSH_COMMANDS="$test_root/ssh-commands"
: >"$calls"
: >"$MUTATION_SSH_COMMANDS"

expect_fail() {
    expected=$1
    shift
    if "$@" >"$test_root/out" 2>"$test_root/err"; then
        printf 'expected failure: %s\n' "$*" >&2
        exit 1
    fi
    grep -F "$expected" "$test_root/err" >/dev/null
}

mcp_cancel() {
    id=$1
    expected=$2
    {
        printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"offline-test","version":"1"}}}'
        printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}'
        printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"slurm_cancel_job\",\"arguments\":{\"cluster\":\"local-label\",\"job_id\":\"$id\",\"expected_job_name\":\"$expected\"}}}"
    } | "$binary" mcp
}

# A bare numeric array master is queued and owner-filtered, but its structured
# ArrayJobId/ArrayTaskId proves that scancel 700 would cancel the whole array.
mcp_cancel 700 array-train | grep -F 'array master' >/dev/null
! grep -F 'local-scancel' "$calls" >/dev/null

# A single proven task is accepted and the controller binding reaches squeue,
# scontrol, and scancel.
mcp_cancel 700_3 array-train | grep -F '"cancelled":true' >/dev/null
grep -F 'local-squeue -h -u offline -j 700_3 -o %i|%T|%j|%M|%R|%P|%S|%Q|%o|%u --clusters local-controller' "$calls" >/dev/null
grep -F 'local-scontrol --cluster local-controller show job -o 700_3' "$calls" >/dev/null
grep -F 'local-scancel --clusters local-controller 700_3' "$calls" >/dev/null

# The pre-preview routing check rejects a script that selects another Slurm
# controller before the fake SSH endpoint has an opportunity to submit it.
: >"$MUTATION_SSH_COMMANDS"
mcp_preview() {
    script=$1
    {
        printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"offline-test","version":"1"}}}'
        printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}'
        printf '%s\n' "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"slurm_preview_submission\",\"arguments\":{\"cluster\":\"remote-label\",\"script\":\"$script\"}}}"
    } | "$binary" mcp
}
mcp_preview Bank/routed-away.sbatch | grep -F 'script routing directive selects controller' >/dev/null
test ! -s "$MUTATION_SSH_COMMANDS"
mcp_preview Bank/short-routed-away.sbatch | grep -F 'script routing directive selects controller' >/dev/null
test ! -s "$MUTATION_SSH_COMMANDS"

# A remote sbatch response is meaningful only when it names the configured
# controller. Missing and mismatched controller identities fail closed.
export MUTATION_SSH_RESPONSE='9001;wrong-controller'
expect_fail 'not configured controller' "$binary" submit Bank/remote.sbatch --cluster remote-label
export MUTATION_SSH_RESPONSE='9001'
expect_fail 'did not return a controller identity' "$binary" submit Bank/remote.sbatch --cluster remote-label
export MUTATION_SSH_RESPONSE='9001;controller-a'
"$binary" submit Bank/remote.sbatch --cluster remote-label | grep -F 'remote-label:9001' >/dev/null

# Query, cancel, and submit all target controller-a. The remote command is
# built with absolute env/shell paths and a fixed PATH, not an expanded login
# PATH, even though SSH itself is a hostile local fixture.
"$binary" cancel 9001 --cluster remote-label >/dev/null
grep -F -- '--clusters controller-a' "$MUTATION_SSH_COMMANDS" >/dev/null
grep -F -- '--cluster controller-a' "$MUTATION_SSH_COMMANDS" >/dev/null
grep -F '/usr/bin/env -i PATH=/usr/local/bin:/usr/bin:/bin HOME=/ /bin/sh -c' "$MUTATION_SSH_COMMANDS" >/dev/null
! grep -F '${PATH' "$MUTATION_SSH_COMMANDS" >/dev/null
! grep -F '$PATH' "$MUTATION_SSH_COMMANDS" >/dev/null

printf 'mutation_bindings: ok (array scope, controller identity, fixed remote PATH; fully offline)\n'
