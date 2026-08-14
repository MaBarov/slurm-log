#!/bin/sh
# End-to-end coverage for the newer MCP tools: pending diagnosis, waiting,
# bundle staging, preflight, resubmit, adoption, doctor, and bank refresh.
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
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
        ;;
      *'|%u'*)
        printf '123|RUNNING|mcp-train|00:01|node-a|gpu|2026-08-13T10:00:00|100|train.sbatch|offline\n'
        printf '124|RUNNING|pending-log|00:01|node-b|cpu|2026-08-13T10:00:00|90|pending.sbatch|offline\n'
        printf '125|RUNNING|no-stdout|00:01|node-c|cpu|2026-08-13T10:00:00|80|quiet.sbatch|offline\n'
        printf '126|RUNNING|outside-stdout|00:01|node-d|cpu|2026-08-13T10:00:00|70|outside.sbatch|offline\n'
        printf '127|RUNNING|array-master|00:01|node-a|gpu|2026-08-13T10:00:00|100|array.sbatch|offline\n'
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

# Initialize the session before the first tool call.
initialized=$(request '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"offline-client","version":"1"}}}')
printf '%s\n' "$initialized" | grep -F '"protocolVersion":"2025-11-25"' >/dev/null
printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}' >&3
request '{"jsonrpc":"2.0","id":2,"method":"ping"}' | grep -F '"result":{}' >/dev/null

# Record a producer hash for job 9001 so resubmission can bind to it.
preview=$(request '{"jsonrpc":"2.0","id":17,"method":"tools/call","params":{"name":"slurm_preview_submission","arguments":{"cluster":"alpha","script":"Bank/train.sbatch"}}}')
token=$(printf '%s\n' "$preview" | sed -n 's/.*"preview_token":"\([^"]*\)".*/\1/p')
test -n "$token"
request "{\"jsonrpc\":\"2.0\",\"id\":18,\"method\":\"tools/call\",\"params\":{\"name\":\"slurm_submit_job\",\"arguments\":{\"preview_token\":\"$token\"}}}" | grep -F '"job_id":"9001"' >/dev/null


# Pending-job explanation pulls partition availability from sinfo and skips
# malformed or default-partition rows.
printf '128|PENDING|pending-train|0:00|Resources|gpu|N/A|42|train.sbatch\n' >"$MCP_QUEUE_OVERRIDE"
pending=$(request '{"jsonrpc":"2.0","id":1701,"method":"tools/call","params":{"name":"slurm_explain_pending","arguments":{"cluster":"alpha","job_id":"128"}}}')
printf '%s\n' "$pending" | grep -F '"state":"PENDING"' >/dev/null
printf '%s\n' "$pending" | grep -F '"requested_partition":"gpu"' >/dev/null
printf '%s\n' "$pending" | grep -F '"partition":"cpu"' >/dev/null
printf '%s\n' "$pending" | grep -F '"reason":"Resources"' >/dev/null
rm "$MCP_QUEUE_OVERRIDE"
request '{"jsonrpc":"2.0","id":1702,"method":"tools/call","params":{"name":"slurm_explain_pending","arguments":{"cluster":"alpha","job_id":"123"}}}' | grep -F 'is not pending' >/dev/null

# slurm_wait_job validates conditions, detects an already-terminal job,
# times out against a still-running job, and observes state/log transitions.
request '{"jsonrpc":"2.0","id":1710,"method":"tools/call","params":{"name":"slurm_wait_job","arguments":{"cluster":"alpha","job_id":"123","until":"bogus"}}}' | grep -F 'invalid wait condition bogus' >/dev/null
printf '9001|COMPLETED|mcp-train|00:01|None|gpu|2026-08-13T10:01:00|100|train.sbatch\n' >"$MCP_QUEUE_OVERRIDE"
done_wait=$(request '{"jsonrpc":"2.0","id":1711,"method":"tools/call","params":{"name":"slurm_wait_job","arguments":{"cluster":"alpha","job_id":"9001","until":"completion","timeout_seconds":5,"interval_seconds":1}}}')
printf '%s\n' "$done_wait" | grep -F '"completed":true' >/dev/null
printf '%s\n' "$done_wait" | grep -F '"timed_out":false' >/dev/null
rm "$MCP_QUEUE_OVERRIDE"
timeout_wait=$(request '{"jsonrpc":"2.0","id":1712,"method":"tools/call","params":{"name":"slurm_wait_job","arguments":{"cluster":"alpha","job_id":"123","until":"completion","timeout_seconds":1,"interval_seconds":1}}}')
printf '%s\n' "$timeout_wait" | grep -F '"timed_out":true' >/dev/null
printf '%s\n' "$timeout_wait" | grep -F '"polls":1' >/dev/null
printf '9001|RUNNING|mcp-train|00:01|node-a|gpu|2026-08-13T10:00:00|100|train.sbatch\n' >"$MCP_QUEUE_OVERRIDE"
( sleep 2; printf '9001|COMPLETED|mcp-train|00:01|None|gpu|2026-08-13T10:01:00|100|train.sbatch\n' >"$MCP_QUEUE_OVERRIDE" ) &
flipper=$!
state_wait=$(request '{"jsonrpc":"2.0","id":1713,"method":"tools/call","params":{"name":"slurm_wait_job","arguments":{"cluster":"alpha","job_id":"9001","until":"state_change","timeout_seconds":10,"interval_seconds":1}}}')
wait "$flipper"
printf '%s\n' "$state_wait" | grep -F '"changed":true' >/dev/null
printf '%s\n' "$state_wait" | grep -F '"final_state":"COMPLETED"' >/dev/null
rm "$MCP_QUEUE_OVERRIDE"
( sleep 2; printf 'log-change marker\n' >>"$log_file" ) &
appender=$!
log_wait=$(request '{"jsonrpc":"2.0","id":1714,"method":"tools/call","params":{"name":"slurm_wait_job","arguments":{"cluster":"alpha","job_id":"123","until":"log_change","timeout_seconds":10,"interval_seconds":1}}}')
wait "$appender"
printf '%s\n' "$log_wait" | grep -F '"changed":true' >/dev/null
printf '9001|RUNNING|mcp-train|00:01|node-a|gpu|2026-08-13T10:00:00|100|train.sbatch\n' >"$MCP_QUEUE_OVERRIDE"
( sleep 2; rm "$MCP_QUEUE_OVERRIDE" ) &
eraser=$!
err_wait=$(request '{"jsonrpc":"2.0","id":1715,"method":"tools/call","params":{"name":"slurm_wait_job","arguments":{"cluster":"alpha","job_id":"9001","until":"state_change","timeout_seconds":10,"interval_seconds":1}}}')
wait "$eraser"
printf '%s\n' "$err_wait" | grep -F '"changed":true' >/dev/null
printf '%s\n' "$err_wait" | grep -F '"completed":true' >/dev/null

# Content-addressed bundle staging writes an immutable local copy, reports the
# remote path for remote destinations, and rejects unknown banks and entries.
mkdir -p "$test_root/bank/data"
printf '{"epoch":1}\n' >"$test_root/bank/data/epoch.json"
printf 'manifest\n' >"$test_root/bank/manifest.txt"
printf 'token\n' >"$test_root/bank/credentials"
bundle=$(request '{"jsonrpc":"2.0","id":1720,"method":"tools/call","params":{"name":"slurm_stage_bundle","arguments":{"bank":"Bank","entries":["data/epoch.json","manifest.txt"],"destination":"local"}}}')
printf '%s\n' "$bundle" | grep -F '"entry_count":2' >/dev/null
printf '%s\n' "$bundle" | grep -F '"destination":"local"' >/dev/null
printf '%s\n' "$bundle" | grep -F '"execution_approved":false' >/dev/null
bundle_sha=$(printf '%s\n' "$bundle" | sed -n 's/.*"bundle_sha256":"\([^"]*\)".*/\1/p')
test -n "$bundle_sha"
test -f "$state_dir/bundles/$bundle_sha.bundle"
remote=$(request '{"jsonrpc":"2.0","id":1721,"method":"tools/call","params":{"name":"slurm_stage_bundle","arguments":{"bank":"Bank","entries":["data/epoch.json"]}}}')
printf '%s\n' "$remote" | grep -F '"destination":"remote"' >/dev/null
printf '%s\n' "$remote" | grep -F '~/.cache/slurm-log/bundles/' >/dev/null
request '{"jsonrpc":"2.0","id":1722,"method":"tools/call","params":{"name":"slurm_stage_bundle","arguments":{"bank":"Nope","entries":["data/epoch.json"]}}}' | grep -F 'ambiguous or unknown' >/dev/null
request '{"jsonrpc":"2.0","id":1723,"method":"tools/call","params":{"name":"slurm_stage_bundle","arguments":{"bank":"Bank","entries":["missing.txt"]}}}' | grep -F 'open bundle entry' >/dev/null
request '{"jsonrpc":"2.0","id":1724,"method":"tools/call","params":{"name":"slurm_stage_bundle","arguments":{"bank":"Bank","entries":["credentials"]}}}' | grep -F 'prohibited path component' >/dev/null

# A tiny same-partition probe job verifies preflight; a still-running probe is
# cancelled after the deadline and its result is marked untrusted.
printf '9001|RUNNING|SLURM_LOG_PREFLIGHT_000000000000|00:01|node-a|gpu|2026-08-13T10:00:00|100|preflight.sbatch\n' >"$MCP_QUEUE_OVERRIDE"
preflight=$(request '{"jsonrpc":"2.0","id":1730,"method":"tools/call","params":{"name":"slurm_preflight_job","arguments":{"cluster":"alpha","script":"Bank/train.sbatch","wait_seconds":2}}}')
printf '%s\n' "$preflight" | grep -F '"cancelled":true' >/dev/null
printf '%s\n' "$preflight" | grep -F '"untrusted_data":true' >/dev/null
grep -F 'SLURM_LOG_PREFLIGHT_' "$MCP_SUBMITTED" >/dev/null
grep -F '9001' "$MCP_CANCELLED" >/dev/null
rm "$MCP_QUEUE_OVERRIDE"
request '{"jsonrpc":"2.0","id":1731,"method":"tools/call","params":{"name":"slurm_preflight_job","arguments":{"cluster":"nope","script":"Bank/train.sbatch"}}}' | grep -F 'unknown cluster nope' >/dev/null
request '{"jsonrpc":"2.0","id":1732,"method":"tools/call","params":{"name":"slurm_preflight_job","arguments":{"cluster":"alpha","script":"Missing/x.sbatch"}}}' | grep -F 'not in an eligible configured bank' >/dev/null

# Preview-resubmit binds the recorded producer hash, refuses active jobs,
# and its token submits with the chosen schedule overrides.
printf '9001|COMPLETED|mcp-train|00:01|None|gpu|2026-08-13T10:01:00|100|train.sbatch\n' >"$MCP_QUEUE_OVERRIDE"
resubmit=$(request '{"jsonrpc":"2.0","id":1740,"method":"tools/call","params":{"name":"slurm_preview_resubmit","arguments":{"cluster":"alpha","job_id":"9001","script":"Bank/train.sbatch","schedule_overrides":{"partition":"cpu"}}}}')
printf '%s\n' "$resubmit" | grep -F '"resubmit":true' >/dev/null
printf '%s\n' "$resubmit" | grep -F '"job_name":"mcp-train"' >/dev/null
rtoken=$(printf '%s\n' "$resubmit" | sed -n 's/.*"preview_token":"\([^"]*\)".*/\1/p')
test -n "$rtoken"
request "{\"jsonrpc\":\"2.0\",\"id\":1741,\"method\":\"tools/call\",\"params\":{\"name\":\"slurm_submit_job\",\"arguments\":{\"preview_token\":\"$rtoken\"}}}" | grep -F '"job_id":"9001"' >/dev/null
grep -F -- '--partition=cpu' "$MCP_SUBMITTED" >/dev/null
rm "$MCP_QUEUE_OVERRIDE"
request '{"jsonrpc":"2.0","id":1742,"method":"tools/call","params":{"name":"slurm_preview_resubmit","arguments":{"cluster":"alpha","job_id":"123","script":"Bank/train.sbatch"}}}' | grep -F 'still active' >/dev/null
request '{"jsonrpc":"2.0","id":1743,"method":"tools/call","params":{"name":"slurm_preview_resubmit","arguments":{"cluster":"alpha","job_id":"bad id","script":"Bank/train.sbatch"}}}' | grep -F 'invalid job ID' >/dev/null
request '{"jsonrpc":"2.0","id":1744,"method":"tools/call","params":{"name":"slurm_preview_resubmit","arguments":{"cluster":"nope","job_id":"9001","script":"Bank/train.sbatch"}}}' | grep -F 'unknown cluster nope' >/dev/null
printf '9001|COMPLETED|renamed-job|00:01|None|gpu|2026-08-13T10:01:00|100|train.sbatch\n' >"$MCP_QUEUE_OVERRIDE"
request '{"jsonrpc":"2.0","id":1745,"method":"tools/call","params":{"name":"slurm_preview_resubmit","arguments":{"cluster":"alpha","job_id":"9001","script":"Bank/train.sbatch"}}}' | grep -F 'job name mismatch' >/dev/null
printf '9001|COMPLETED|mcp-train|00:01|None|gpu|2026-08-13T10:01:00|100|train.sbatch\n' >"$MCP_QUEUE_OVERRIDE"
printf '# changed after the recorded submission\n' >>"$test_root/bank/train.sbatch"
request '{"jsonrpc":"2.0","id":1746,"method":"tools/call","params":{"name":"slurm_preview_resubmit","arguments":{"cluster":"alpha","job_id":"9001","script":"Bank/train.sbatch"}}}' | grep -F 'no longer matches the bank script' >/dev/null
cp "$test_root/original.sbatch" "$test_root/bank/train.sbatch"
rm "$MCP_QUEUE_OVERRIDE"
request '{"jsonrpc":"2.0","id":1747,"method":"tools/call","params":{"name":"slurm_preview_resubmit","arguments":{"cluster":"alpha","job_id":"9001","script":"Bank/train.sbatch"}}}' | grep -F 'not an active job owned by the configured user' >/dev/null

# Adoption records externally submitted jobs and never claims a preview
# authorized them; invalid hashes are dropped, unknown jobs fail closed.
adopt=$(request '{"jsonrpc":"2.0","id":1750,"method":"tools/call","params":{"name":"slurm_adopt_job","arguments":{"cluster":"alpha","job_id":"124","batch_script_sha256":"0000000000000000000000000000000000000000000000000000000000000007"}}}')
printf '%s\n' "$adopt" | grep -F '"adopted":true' >/dev/null
printf '%s\n' "$adopt" | grep -F '"externally_submitted":true' >/dev/null
printf '%s\n' "$adopt" | grep -F '"preview_authorized":false' >/dev/null
invalid_hash=$(request '{"jsonrpc":"2.0","id":1751,"method":"tools/call","params":{"name":"slurm_adopt_job","arguments":{"cluster":"alpha","job_id":"125","batch_script_sha256":"not-hex"}}}')
printf '%s\n' "$invalid_hash" | grep -F 'must be a 64-character lowercase hex digest' >/dev/null
request '{"jsonrpc":"2.0","id":1752,"method":"tools/call","params":{"name":"slurm_adopt_job","arguments":{"cluster":"alpha","job_id":"9001"}}}' | grep -F 'not an active job owned by the configured user' >/dev/null

# Doctor distinguishes a healthy bank from a healthy scheduler, and reports a
# cluster whose scheduler transport fails without failing the whole tool.
touch "$MCP_SSH_FAIL"
doctor_broken=$(request '{"jsonrpc":"2.0","id":1761,"method":"tools/call","params":{"name":"slurm_doctor","arguments":{}}}')
printf '%s\n' "$doctor_broken" | grep -F '"scheduler_healthy":false' >/dev/null
printf '%s\n' "$doctor_broken" | grep -F '"scheduler_reachable":false' >/dev/null
request '{"jsonrpc":"2.0","id":1760,"method":"tools/call","params":{"name":"slurm_list_clusters","arguments":{}}}' | grep -F '"connectivity":"degraded"' >/dev/null
rm "$MCP_SSH_FAIL"
sleep 11
doctor=$(request '{"jsonrpc":"2.0","id":1762,"method":"tools/call","params":{"name":"slurm_doctor","arguments":{}}}')
printf '%s\n' "$doctor" | grep -F '"bank_healthy":true' >/dev/null
printf '%s\n' "$doctor" | grep -F '"scheduler_healthy":true' >/dev/null
printf '%s\n' "$doctor" | grep -F '"scheduler_reachable":true' >/dev/null
printf '%s\n' "$doctor" | grep -F '"name":"Bank"' >/dev/null
printf '%s\n' "$doctor" | grep -F '"indexed_script_count":' >/dev/null
refresh=$(request '{"jsonrpc":"2.0","id":1763,"method":"tools/call","params":{"name":"slurm_refresh_bank","arguments":{}}}')
printf '%s\n' "$refresh" | grep -F '"refreshed":true' >/dev/null
printf '%s\n' "$refresh" | grep -F '"catalog_generation":"' >/dev/null

# Inspected dependencies come from a second controller read.
request '{"jsonrpc":"2.0","id":1764,"method":"tools/call","params":{"name":"slurm_inspect_job","arguments":{"cluster":"alpha","job_id":"124"}}}' | grep -F '"dependencies":["afterok:42","afterany:43"]' >/dev/null

printf 'mcp_tools: ok (pending diagnosis, waiting, staging, preflight, resubmit, adoption, doctor, refresh)\n'
