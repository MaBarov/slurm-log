#!/bin/sh
# Shared harness for the offline stdio MCP integration tests: fake Slurm,
# SSH, and tmux commands, a hermetic config, and the request/receive helpers.
# Sourced by tests/mcp_server.sh; expects $project_dir to be set.
set -eu

binary=${SLURM_LOG_TEST_BINARY:-$project_dir/target/release/slurm-log}
test_root=$(mktemp -d)
case "$test_root" in /tmp/*) ;; *) exit 1 ;; esac
fake_bin=$test_root/bin
state_dir=$test_root/state
mkdir -p "$fake_bin" "$state_dir" "$test_root/bank/nested" "$test_root/work" "$test_root/clients"

cleanup() {
    SLURM_LOG_CONFIG="$test_root/config.json" "$binary" daemon stop >/dev/null 2>&1 || true
    SLURM_LOG_CONFIG="$test_root/shared-config.json" "$binary" daemon stop >/dev/null 2>&1 || true
    rm -rf "$test_root"
}
trap cleanup EXIT HUP INT TERM

log_file=$test_root/work/job.log
foreign_log=$test_root/work/foreign.log
outside_log=$test_root/outside.log
printf 'plain first line\nUserWarning: hidden warning\n' >"$log_file"
printf 'OWNER_ISOLATION_PROOF_SECRET\n' >"$foreign_log"
printf 'OUTSIDE_STDOUT_PROOF_SECRET\n' >"$outside_log"
cat >"$test_root/bank/train.sbatch" <<'EOF'
#!/bin/sh
#SBATCH --job-name=mcp-train
#SBATCH --gpus=1
#SLURM_LOG-RESULT: cpu_gate.json
#SLURM_LOG-RESULT: verifier-receipt.txt
printf never-executed
EOF
# A script with no declared result markers exercises the empty-result bail.
cat >"$test_root/bank/plain.sbatch" <<'EOF'
#!/bin/sh
#SBATCH --job-name=plain
printf never-executed
EOF
cp "$test_root/bank/train.sbatch" "$test_root/original.sbatch"
printf '{"passed":true,"metrics":{"accuracy":0.97}}\n' >"$test_root/work/cpu_gate.json"
# Non-sbatch files and symlinks must be skipped by the bank cache fingerprint.
printf 'scratch notes\n' >"$test_root/bank/notes.txt"
ln -s notes.txt "$test_root/bank/link.txt"

cat >"$fake_bin/squeue" <<'EOF'
#!/bin/sh
env | grep -Eq '^(SBATCH_|SCANCEL_|SLURM_CLUSTERS=)' && exit 41
printf 'squeue %s\n' "$*" >>"$MCP_CALLS"
if test -f "$MCP_QUEUE_OVERRIDE"; then
    case " $* " in
      *" -j "*)
        case "$*" in
          *'%i|%u|%T'*) sed 's/^\([^|]*\)|/\1|offline|/' "$MCP_QUEUE_OVERRIDE" ;;
          *'|%u'*) sed 's/$/|offline/' "$MCP_QUEUE_OVERRIDE" ;;
          *) cat "$MCP_QUEUE_OVERRIDE" ;;
        esac
        ;;
      *'|%u'*) sed 's/$/|offline/' "$MCP_QUEUE_OVERRIDE" ;;
      *) cat "$MCP_QUEUE_OVERRIDE" ;;
    esac
    exit 0
fi
case " $* " in
  *" -j "*)
    case "$*" in
      *'%i|%u|%T'*)
        printf '123|offline|RUNNING|mcp-train|00:01|node-a|gpu|2026-08-13T10:00:00|100|train.sbatch\n'
        printf '124|offline|RUNNING|pending-log|00:01|node-b|cpu|2026-08-13T10:00:00|90|pending.sbatch\n'
        printf '125|offline|RUNNING|no-stdout|00:01|node-c|cpu|2026-08-13T10:00:00|80|quiet.sbatch\n'
        printf '126|offline|RUNNING|outside-stdout|00:01|node-d|cpu|2026-08-13T10:00:00|70|outside.sbatch\n'
        printf '127|offline|RUNNING|array-master|00:01|node-a|gpu|2026-08-13T10:00:00|60|array.sbatch\n'
        printf '128|offline|RUNNING|plain|00:01|node-e|cpu|2026-08-13T10:00:00|50|plain.sbatch\n'
        ;;
      *'|%u'*)
        printf '123|RUNNING|mcp-train|00:01|node-a|gpu|2026-08-13T10:00:00|100|train.sbatch|offline\n'
        printf '124|RUNNING|pending-log|00:01|node-b|cpu|2026-08-13T10:00:00|90|pending.sbatch|offline\n'
        printf '125|RUNNING|no-stdout|00:01|node-c|cpu|2026-08-13T10:00:00|80|quiet.sbatch|offline\n'
        printf '126|RUNNING|outside-stdout|00:01|node-d|cpu|2026-08-13T10:00:00|70|outside.sbatch|offline\n'
        printf '127|RUNNING|array-master|00:01|node-a|gpu|2026-08-13T10:00:00|100|array.sbatch|offline\n'
        printf '128|RUNNING|plain|00:01|node-e|cpu|2026-08-13T10:00:00|50|plain.sbatch|offline\n'
        ;;
      *) exit 42 ;;
    esac
    exit 0
    ;;
esac
printf '123|RUNNING|mcp-train|00:01|node-a|gpu|2026-08-13T10:00:00|100|train.sbatch|offline\n'
printf '124|RUNNING|pending-log|00:01|node-b|cpu|2026-08-13T10:00:00|90|pending.sbatch|offline\n'
printf '125|RUNNING|no-stdout|00:01|node-c|cpu|2026-08-13T10:00:00|80|quiet.sbatch|offline\n'
printf '126|RUNNING|outside-stdout|00:01|node-d|cpu|2026-08-13T10:00:00|70|outside.sbatch|offline\n'
EOF
cat >"$fake_bin/scontrol" <<'EOF'
#!/bin/sh
env | grep -Eq '^(SBATCH_|SCANCEL_|SLURM_CLUSTERS=)' && exit 41
printf 'scontrol %s\n' "$*" >>"$MCP_CALLS"
case "$*" in
  *128*) printf 'JobId=128 UserId=offline(1000) JobName=plain JobState=RUNNING StdOut=%s\n' "$MCP_LOG_FILE" ;;
  *127*) printf 'JobId=127 UserId=offline(1000) JobName=array-master JobState=RUNNING ArrayJobId=127 ArrayTaskId=0-9\n' ;;
  *124*) printf 'JobId=124 UserId=offline(1000) JobName=pending-log JobState=RUNNING StdOut=%s/missing.log Dependency=afterok:42,afterany:43\n' "$(dirname "$MCP_LOG_FILE")" ;;
  *125*) printf 'JobId=125 UserId=offline(1000) JobName=no-stdout JobState=RUNNING StdOut=/dev/null\n' ;;
  *126*) printf 'JobId=126 UserId=offline(1000) JobName=outside-stdout JobState=RUNNING StdOut=%s/../outside.log\n' "$(dirname "$MCP_LOG_FILE")" ;;
  *999*) printf 'JobId=999 UserId=other(2000) JobName=foreign JobState=RUNNING StdOut=%s/foreign.log\n' "$(dirname "$MCP_LOG_FILE")" ;;
  *9001*) printf 'JobId=9001 UserId=offline(1000) JobName=SLURM_LOG_PREFLIGHT_000000000000 JobState=RUNNING StdOut=%s\n' "$MCP_LOG_FILE" ;;
  *) printf 'JobId=123 UserId=offline(1000) JobName=mcp-train JobState=RUNNING Reason=None Partition=gpu StdOut=%s ExitCode=0:0 Dependency=None NumNodes=1 NumCPUs=2\n' "$MCP_LOG_FILE" ;;
esac
EOF
cat >"$fake_bin/sstat" <<'EOF'
#!/bin/sh
exit 0
EOF
cat >"$fake_bin/sinfo" <<'EOF'
#!/bin/sh
env | grep -Eq '^(SBATCH_|SCANCEL_|SLURM_CLUSTERS=)' && exit 41
printf 'sinfo %s\n' "$*" >>"$MCP_CALLS"
printf 'gpu*|up|2|idle\n'
printf 'cpu|up|4|alloc\n'
printf 'malformed-line\n'
printf '|down|1|drained\n'
EOF
cat >"$fake_bin/sbatch" <<'EOF'
#!/bin/sh
env | grep -Eq '^(SBATCH_|SCANCEL_|SLURM_CLUSTERS=)' && exit 41
cat >"$MCP_SUBMITTED"
printf '9001\n'
EOF
cat >"$fake_bin/scancel" <<'EOF'
#!/bin/sh
env | grep -Eq '^(SBATCH_|SCANCEL_|SLURM_CLUSTERS=)' && exit 41
printf '%s\n' "$*" >"$MCP_CANCELLED"
EOF
cat >"$fake_bin/tmux" <<'EOF'
#!/bin/sh
test -f "$MCP_TMUX_FAIL" && exit 1
if test -f "$MCP_TMUX_MALFORMED"; then
    printf 'not-a-slurm-workspace|1|alpha|123\n'
    exit 0
fi
printf 'slurm-logs-alpha|1|alpha|123\n'
EOF
cat >"$fake_bin/ssh" <<'EOF'
#!/bin/sh
test -f "$MCP_SSH_FAIL" && exit 5
for remote do :; done
printf 'ssh %s\n' "$remote" >>"$MCP_CALLS"
case "$remote" in
  *'SLURMLOG|'*)
    test -f "$MCP_LOG_SSH_FAIL" && exit 5
    size=$(wc -c <"$MCP_LOG_FILE")
    modified=$(stat -c %Y "$MCP_LOG_FILE")
    printf 'SLURMLOG|1|2|%s|%s\n' "$size" "$modified"
    case "$remote" in
      *'tail -c '*) cat "$MCP_LOG_FILE" ;;
    esac
    ;;
  *'squeue '*)
    case "$remote" in
      *'%i|%u|%T'*) printf '123|offline|RUNNING|mcp-train|00:01|node-a|gpu|2026-08-13T10:00:00|100|train.sbatch\n' ;;
      *'|%u'*) printf '123|RUNNING|mcp-train|00:01|node-a|gpu|2026-08-13T10:00:00|100|train.sbatch|offline\n' ;;
      *) exit 42 ;;
    esac
    ;;
  *'scontrol '*)
    printf 'JobId=123 UserId=offline(1000) JobName=mcp-train JobState=RUNNING Reason=None Partition=gpu StdOut=%s ExitCode=0:0 Dependency=None NumNodes=1 NumCPUs=2\n' "$MCP_LOG_FILE"
    ;;
  *'sstat '*) : ;;
  *)
    printf 'unexpected fake SSH request: %s\n' "$remote" >&2
    exit 73
    ;;
esac
EOF
chmod 755 "$fake_bin/squeue" "$fake_bin/scontrol" "$fake_bin/sstat" "$fake_bin/sinfo" \
    "$fake_bin/sbatch" "$fake_bin/scancel" "$fake_bin/tmux" "$fake_bin/ssh"

cat >"$test_root/config.json" <<EOF
{
  "clusters": [
    {"name":"alpha","controller":"alpha","transport":"local","user":"offline","workingDirectory":"$test_root/work","accounting":false},
    {"name":"beta","transport":"ssh","user":"offline","sshHost":"fake-cluster","workingDirectory":"$test_root/work","accounting":false}
  ],
  "sbatchBanks": [{"path":"$test_root/bank","name":"Bank"}],
  "statePath": "$state_dir/state.json"
}
EOF
chmod 600 "$test_root/config.json"

export PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin"
export HOME=$test_root/home
export SLURM_LOG_CONFIG=$test_root/config.json
export MCP_LOG_FILE=$log_file
export MCP_CALLS=$test_root/calls
export MCP_SUBMITTED=$test_root/submitted
export MCP_CANCELLED=$test_root/cancelled
export MCP_QUEUE_OVERRIDE=$test_root/queue-override
export MCP_TMUX_FAIL=$test_root/tmux-fail
export MCP_TMUX_MALFORMED=$test_root/tmux-malformed
export MCP_SSH_FAIL=$test_root/ssh-fail
export MCP_LOG_SSH_FAIL=$test_root/log-ssh-fail
export SBATCH_JOB_NAME=ambient-override
export SCANCEL_FULL=1
export SLURM_CLUSTERS=wrong-cluster
: >"$MCP_CALLS"

requests=$test_root/requests
responses=$test_root/responses
mkfifo "$requests" "$responses"
"$binary" mcp <"$requests" >"$responses" 2>"$test_root/mcp.err" &
server_pid=$!
exec 3>"$requests"
exec 4<"$responses"
transcript=$test_root/transcript
: >"$transcript"

receive() {
    IFS= read -r response <&4
    printf '%s\n' "$response" >>"$transcript"
    printf '%s\n' "$response"
}
request() {
    printf '%s\n' "$1" >&3
    receive
}
