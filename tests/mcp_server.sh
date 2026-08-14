#!/bin/sh
# End-to-end stdio MCP, log, mutation, subscription, setup, and sharing test.
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

# Malformed input is ignored without contaminating stdout or killing the server.
printf '{bad\n' >&3
initialized=$(request '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"offline-client","version":"1"}}}')
printf '%s\n' "$initialized" | grep -F '"protocolVersion":"2025-11-25"' >/dev/null
printf '%s\n' "$initialized" | grep -F '"subscribe":true' >/dev/null
printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}' >&3

request '{"jsonrpc":"2.0","id":2,"method":"ping"}' | grep -F '"result":{}' >/dev/null
tools=$(request '{"jsonrpc":"2.0","id":3,"method":"tools/list","params":{}}')
test "$(printf '%s\n' "$tools" | grep -o '"name":"slurm_' | wc -l)" -eq 21
printf '%s\n' "$tools" | grep -F '"enum":["alpha","beta"]' >/dev/null
printf '%s\n' "$tools" | grep -F '"destructiveHint":true' >/dev/null
request '{"jsonrpc":"2.0","id":30,"method":"tools/list","params":{"cursor":"bad"}}' | grep -F 'invalid tool pagination cursor' >/dev/null
request '{"jsonrpc":"2.0","id":4,"method":"unknown/method"}' | grep -F '"code":-32601' >/dev/null
printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":999,"reason":"offline"}}' >&3
request '{"jsonrpc":"2.0","id":5,"method":"ping"}' | grep -F '"result":{}' >/dev/null

# Dynamic schemas reject missing clusters, and exact cluster-qualified IDs stay distinct.
request '{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"slurm_inspect_job","arguments":{"job_id":"123"}}}' | grep -F 'cluster' >/dev/null
alpha=$(request '{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"slurm_list_jobs","arguments":{"cluster":"alpha"}}}')
beta=$(request '{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"slurm_list_jobs","arguments":{"cluster":"beta"}}}')
printf '%s\n' "$alpha" | grep -F '"structuredContent":{"cluster":"alpha"' >/dev/null
printf '%s\n' "$beta" | grep -F '"structuredContent":{"cluster":"beta"' >/dev/null
grep -E 'squeue .* -u offline ' "$MCP_CALLS" >/dev/null
request '{"jsonrpc":"2.0","id":70,"method":"tools/call","params":{"name":"slurm_list_clusters","arguments":{}}}' | grep -F '"transport":"ssh"' >/dev/null
request '{"jsonrpc":"2.0","id":71,"method":"tools/call","params":{"name":"slurm_inspect_job","arguments":{"cluster":"alpha","job_id":"123"}}}' | grep -F '"dependencies":[]' >/dev/null
request '{"jsonrpc":"2.0","id":72,"method":"tools/call","params":{"name":"slurm_workspace_context","arguments":{}}}' | grep -F 'slurm-logs-alpha' >/dev/null
touch "$MCP_TMUX_FAIL"
request '{"jsonrpc":"2.0","id":721,"method":"tools/call","params":{"name":"slurm_workspace_context","arguments":{}}}' | grep -F '"workspaces":[]' >/dev/null
rm "$MCP_TMUX_FAIL"
chmod 644 "$fake_bin/tmux"
request '{"jsonrpc":"2.0","id":722,"method":"tools/call","params":{"name":"slurm_workspace_context","arguments":{}}}' | grep -F '"workspaces":[]' >/dev/null
chmod 755 "$fake_bin/tmux"
touch "$MCP_TMUX_MALFORMED"
request '{"jsonrpc":"2.0","id":723,"method":"tools/call","params":{"name":"slurm_workspace_context","arguments":{}}}' | grep -F '"focused_jobs":[]' >/dev/null
rm "$MCP_TMUX_MALFORMED"
request '{"jsonrpc":"2.0","id":724,"method":"tools/call","params":{"name":"slurm_unknown_tool","arguments":{}}}' | grep -F 'unknown tool slurm_unknown_tool' >/dev/null
script_result=$(request '{"jsonrpc":"2.0","id":73,"method":"tools/call","params":{"name":"slurm_list_scripts","arguments":{"cluster":"alpha","search":"train","limit":1}}}')
printf '%s\n' "$script_result" | grep -F 'Bank/train.sbatch' >/dev/null
printf '%s\n' "$script_result" | grep -F '1 result(s); script: mcp-train' >/dev/null
request '{"jsonrpc":"2.0","id":74,"method":"tools/call","params":{"name":"slurm_list_jobs","arguments":{"cluster":"alpha","history":"all","states":["RUNNING"],"include_blocked":true,"search":"mcp","limit":1}}}' | grep -F 'mcp-train' >/dev/null

# A connected MCP process discovers nested scripts and newly configured banks
# without reconnecting, while its cluster schema remains the startup snapshot.
cat >"$test_root/bank/nested/live-added.sbatch" <<'EOF'
#!/bin/sh
#SBATCH --job-name=live-added
EOF
request '{"jsonrpc":"2.0","id":75,"method":"tools/call","params":{"name":"slurm_list_scripts","arguments":{"cluster":"alpha","search":"live-added"}}}' | grep -F 'Bank/nested/live-added.sbatch' >/dev/null
mkdir -p "$test_root/added-bank"
cat >"$test_root/added-bank/config-added.sbatch" <<'EOF'
#!/bin/sh
#SBATCH --job-name=config-added
EOF
cat >"$test_root/config.next.json" <<EOF
{
  "clusters": [
    {"name":"alpha","controller":"alpha","transport":"local","user":"offline","workingDirectory":"$test_root/work","accounting":false},
    {"name":"beta","transport":"ssh","user":"offline","sshHost":"fake-cluster","workingDirectory":"$test_root/work","accounting":false}
  ],
  "sbatchBanks": [
    {"path":"$test_root/bank","name":"Bank"},
    {"path":"$test_root/added-bank","name":"Added"}
  ],
  "statePath": "$state_dir/state.json"
}
EOF
chmod 600 "$test_root/config.next.json"
mv "$test_root/config.next.json" "$test_root/config.json"
request '{"jsonrpc":"2.0","id":76,"method":"tools/call","params":{"name":"slurm_list_scripts","arguments":{"cluster":"alpha","search":"config-added"}}}' | grep -F 'Added/config-added.sbatch' >/dev/null

# Resources/templates expose only concrete slurm-log URIs.
request '{"jsonrpc":"2.0","id":9,"method":"resources/list","params":{}}' | grep -F 'slurm-log://jobs/alpha/123' >/dev/null
request '{"jsonrpc":"2.0","id":901,"method":"resources/list","params":{"cursor":"r:1"}}' | grep -F 'slurm-log://clusters/alpha/jobs' >/dev/null
request '{"jsonrpc":"2.0","id":902,"method":"resources/list","params":{"cursor":"bad"}}' | grep -F 'invalid resource cursor' >/dev/null
request '{"jsonrpc":"2.0","id":10,"method":"resources/templates/list","params":{}}' | grep -F 'slurm-log://jobs/{cluster}/{job_id}/log' >/dev/null
request '{"jsonrpc":"2.0","id":11,"method":"resources/read","params":{"uri":"file:///etc/passwd"}}' | grep -F '"code":-32602' >/dev/null
request '{"jsonrpc":"2.0","id":90,"method":"resources/read","params":{"uri":"slurm-log://clusters"}}' | grep -F 'alpha' >/dev/null
request '{"jsonrpc":"2.0","id":91,"method":"resources/read","params":{"uri":"slurm-log://clusters/alpha/jobs"}}' | grep -F 'mcp-train' >/dev/null
request '{"jsonrpc":"2.0","id":92,"method":"resources/read","params":{"uri":"slurm-log://jobs/alpha/123"}}' | grep -F 'job_id' >/dev/null
request '{"jsonrpc":"2.0","id":93,"method":"resources/read","params":{"uri":"slurm-log://jobs/alpha/123/details"}}' | grep -F 'details' >/dev/null
request '{"jsonrpc":"2.0","id":94,"method":"resources/read","params":{"uri":"slurm-log://jobs/alpha/123/log"}}' | grep -F 'plain first line' >/dev/null
request '{"jsonrpc":"2.0","id":110,"method":"tools/call","params":{"name":"slurm_read_log","arguments":{"cluster":"alpha","job_id":"123","path":"/etc/passwd"}}}' | grep -F 'unknown argument path' >/dev/null
request '{"jsonrpc":"2.0","id":111,"method":"tools/call","params":{"name":"slurm_preview_submission","arguments":{"cluster":"alpha","script":"Bank/train.sbatch","script_body":"#!/bin/sh"}}}' | grep -F 'unknown argument script_body' >/dev/null

# Tail, incremental append, truncation, rotation, sanitization, search, and diagnosis.
tail_result=$(request '{"jsonrpc":"2.0","id":12,"method":"tools/call","params":{"name":"slurm_read_log","arguments":{"cluster":"alpha","job_id":"123","filter":"hide_warnings"}}}')
printf '%s\n' "$tail_result" | grep -F 'plain first line' >/dev/null
! printf '%s\n' "$tail_result" | grep -F 'hidden warning' >/dev/null
cursor=$(printf '%s\n' "$tail_result" | sed -n 's/.*"next_cursor":"\([^"]*\)".*/\1/p')
test -n "$cursor"
request '{"jsonrpc":"2.0","id":120,"method":"tools/call","params":{"name":"slurm_read_log","arguments":{"cluster":"beta","job_id":"123","filter":"warnings"}}}' | grep -F 'hidden warning' >/dev/null
request '{"jsonrpc":"2.0","id":121,"method":"tools/call","params":{"name":"slurm_read_log","arguments":{"cluster":"alpha","job_id":"124"}}}' | grep -F '"status":"pending_log"' >/dev/null
request '{"jsonrpc":"2.0","id":122,"method":"tools/call","params":{"name":"slurm_read_log","arguments":{"cluster":"alpha","job_id":"125"}}}' | grep -F '"status":"no_stdout"' >/dev/null
request '{"jsonrpc":"2.0","id":1220,"method":"tools/call","params":{"name":"slurm_read_log","arguments":{"cluster":"alpha","job_id":"123","filter":"bogus"}}}' | grep -F 'invalid log filter bogus' >/dev/null
outside=$(request '{"jsonrpc":"2.0","id":1221,"method":"tools/call","params":{"name":"slurm_read_log","arguments":{"cluster":"alpha","job_id":"126"}}}')
printf '%s\n' "$outside" | grep -F '"status":"no_stdout"' >/dev/null
! printf '%s\n' "$outside" | grep -F 'OUTSIDE_STDOUT_PROOF_SECRET' >/dev/null
foreign=$(request '{"jsonrpc":"2.0","id":123,"method":"tools/call","params":{"name":"slurm_read_log","arguments":{"cluster":"alpha","job_id":"999"}}}')
printf '%s\n' "$foreign" | grep -F 'owned by the configured' >/dev/null
! printf '%s\n' "$foreign" | grep -F 'OWNER_ISOLATION_PROOF_SECRET' >/dev/null
printf 'Traceback (most recent call last):\nValueError: appended\n' >>"$log_file"
sleep 6
incremental=$(request "{\"jsonrpc\":\"2.0\",\"id\":13,\"method\":\"tools/call\",\"params\":{\"name\":\"slurm_read_log\",\"arguments\":{\"cluster\":\"alpha\",\"job_id\":\"123\",\"cursor\":\"$cursor\",\"filter\":\"all\"}}}")
printf '%s\n' "$incremental" | grep -F 'ValueError: appended' >/dev/null
next_cursor=$(printf '%s\n' "$incremental" | sed -n 's/.*"next_cursor":"\([^"]*\)".*/\1/p')
printf 'short\n' >"$log_file"
sleep 6
reset=$(request "{\"jsonrpc\":\"2.0\",\"id\":14,\"method\":\"tools/call\",\"params\":{\"name\":\"slurm_read_log\",\"arguments\":{\"cluster\":\"alpha\",\"job_id\":\"123\",\"cursor\":\"$next_cursor\",\"filter\":\"all\"}}}")
printf '%s\n' "$reset" | grep -F '"cursor_reset":true' >/dev/null
reset_cursor=$(printf '%s\n' "$reset" | sed -n 's/.*"next_cursor":"\([^"]*\)".*/\1/p')
mv "$log_file" "$test_root/job.log.1"
printf 'rotated generation\n' >"$log_file"
sleep 6
rotated=$(request "{\"jsonrpc\":\"2.0\",\"id\":140,\"method\":\"tools/call\",\"params\":{\"name\":\"slurm_read_log\",\"arguments\":{\"cluster\":\"alpha\",\"job_id\":\"123\",\"cursor\":\"$reset_cursor\",\"filter\":\"all\"}}}")
printf '%s\n' "$rotated" | grep -F '"cursor_reset":true' >/dev/null
printf '%s\n' "$rotated" | grep -F 'rotated generation' >/dev/null
printf 'CUDA out of memory on rank 0\nNCCL error\n' >>"$log_file"
sleep 6
request '{"jsonrpc":"2.0","id":15,"method":"tools/call","params":{"name":"slurm_search_log","arguments":{"cluster":"alpha","job_id":"123","pattern":"(a+)+$","regex":true,"max_matches":5}}}' | grep -F '"scan_limit_bytes":4194304' >/dev/null
literal_search=$(request '{"jsonrpc":"2.0","id":151,"method":"tools/call","params":{"name":"slurm_search_log","arguments":{"cluster":"alpha","job_id":"123","pattern":"CUDA out of memory","context_lines":1,"max_matches":1}}}')
printf '%s\n' "$literal_search" | grep -F '"match_count":1' >/dev/null
printf '%s\n' "$literal_search" | grep -F '"matched":true' >/dev/null
printf '%s\n' "$literal_search" | grep -F 'NCCL error' >/dev/null
regex_search=$(request '{"jsonrpc":"2.0","id":152,"method":"tools/call","params":{"name":"slurm_search_log","arguments":{"cluster":"alpha","job_id":"123","pattern":"NCCL[ ]+error","regex":true,"context_lines":0}}}')
printf '%s\n' "$regex_search" | grep -F '"match_count":1' >/dev/null
request '{"jsonrpc":"2.0","id":16,"method":"tools/call","params":{"name":"slurm_diagnose_job","arguments":{"cluster":"alpha","job_id":"123"}}}' | grep -F 'cuda_out_of_memory' >/dev/null

# Artifact reads are bound to what the batch script declared, and the
# declared-result reader exposes exactly those files (cpu_gate.json/verifier
# receipts) under the same confinement.
request '{"jsonrpc":"2.0","id":1601,"method":"tools/call","params":{"name":"slurm_find_artifact","arguments":{"cluster":"alpha","job_id":"123","pattern":"cpu_gate.json"}}}' | grep -F '\"passed\":true' >/dev/null
request '{"jsonrpc":"2.0","id":1602,"method":"tools/call","params":{"name":"slurm_find_artifact","arguments":{"cluster":"alpha","job_id":"123","pattern":"undeclared.txt"}}}' | grep -F 'not declared' >/dev/null
request '{"jsonrpc":"2.0","id":1603,"method":"tools/call","params":{"name":"slurm_find_artifact","arguments":{"cluster":"beta","job_id":"123","pattern":"cpu_gate.json"}}}' | grep -F 'local cluster working directory' >/dev/null
declared=$(request '{"jsonrpc":"2.0","id":1604,"method":"tools/call","params":{"name":"slurm_read_declared_result","arguments":{"cluster":"alpha","job_id":"123","result":"cpu_gate.json"}}}')
printf '%s\n' "$declared" | grep -F '"declared_results":["cpu_gate.json","verifier-receipt.txt"]' >/dev/null
printf '%s\n' "$declared" | grep -F '"total":1' >/dev/null
printf '%s\n' "$declared" | grep -F '\"passed\":true' >/dev/null
request '{"jsonrpc":"2.0","id":1605,"method":"tools/call","params":{"name":"slurm_read_declared_result","arguments":{"cluster":"alpha","job_id":"123","result":"other.json"}}}' | grep -F 'not a declared result' >/dev/null
request '{"jsonrpc":"2.0","id":1606,"method":"tools/call","params":{"name":"slurm_read_declared_result","arguments":{"cluster":"alpha","job_id":"125"}}}' | grep -F 'no configured script records this job' >/dev/null

# Submission is digest-bound, one-use, exact-stdin only; cancellation revalidates name.
stale_preview=$(request '{"jsonrpc":"2.0","id":160,"method":"tools/call","params":{"name":"slurm_preview_submission","arguments":{"cluster":"alpha","script":"Bank/train.sbatch"}}}')
stale_token=$(printf '%s\n' "$stale_preview" | sed -n 's/.*"preview_token":"\([^"]*\)".*/\1/p')
printf '# changed after preview\n' >>"$test_root/bank/train.sbatch"
request "{\"jsonrpc\":\"2.0\",\"id\":161,\"method\":\"tools/call\",\"params\":{\"name\":\"slurm_submit_job\",\"arguments\":{\"preview_token\":\"$stale_token\"}}}" | grep -F 'preview is stale' >/dev/null
cp "$test_root/original.sbatch" "$test_root/bank/train.sbatch"
preview=$(request '{"jsonrpc":"2.0","id":17,"method":"tools/call","params":{"name":"slurm_preview_submission","arguments":{"cluster":"alpha","script":"Bank/train.sbatch"}}}')
token=$(printf '%s\n' "$preview" | sed -n 's/.*"preview_token":"\([^"]*\)".*/\1/p')
test -n "$token"
request "{\"jsonrpc\":\"2.0\",\"id\":18,\"method\":\"tools/call\",\"params\":{\"name\":\"slurm_submit_job\",\"arguments\":{\"preview_token\":\"$token\"}}}" | grep -F '"job_id":"9001"' >/dev/null
cmp "$test_root/bank/train.sbatch" "$MCP_SUBMITTED"
request "{\"jsonrpc\":\"2.0\",\"id\":19,\"method\":\"tools/call\",\"params\":{\"name\":\"slurm_submit_job\",\"arguments\":{\"preview_token\":\"$token\"}}}" | grep -F 'already consumed' >/dev/null
request '{"jsonrpc":"2.0","id":20,"method":"tools/call","params":{"name":"slurm_cancel_job","arguments":{"cluster":"alpha","job_id":"123","expected_job_name":"wrong"}}}' | grep -F 'job name changed' >/dev/null
request '{"jsonrpc":"2.0","id":201,"method":"tools/call","params":{"name":"slurm_cancel_job","arguments":{"cluster":"alpha","job_id":"127","expected_job_name":"array-master"}}}' | grep -F 'array master' >/dev/null
printf '123|RUNNING|replaced-name|00:01|node-a|gpu|2026-08-13T10:00:00|100|train.sbatch\n' >"$MCP_QUEUE_OVERRIDE"
request '{"jsonrpc":"2.0","id":202,"method":"tools/call","params":{"name":"slurm_cancel_job","arguments":{"cluster":"alpha","job_id":"123","expected_job_name":"mcp-train"}}}' | grep -F 'disagree about job name or state' >/dev/null
rm "$MCP_QUEUE_OVERRIDE"
request '{"jsonrpc":"2.0","id":21,"method":"tools/call","params":{"name":"slurm_cancel_job","arguments":{"cluster":"alpha","job_id":"123","expected_job_name":"mcp-train"}}}' | grep -F '"cancelled":true' >/dev/null
test "$(cat "$MCP_CANCELLED")" = '--clusters alpha 123'

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
audit=$state_dir/mcp-audit.jsonl
test "$(stat -c %a "$audit")" = 600
grep -F '"digest":"' "$audit" >/dev/null
! grep -F 'never-executed' "$audit" >/dev/null

# A concrete log subscription emits only after change, then can be removed.
request '{"jsonrpc":"2.0","id":210,"method":"resources/subscribe","params":{"uri":"slurm-log://clusters"}}' | grep -F '"result":{}' >/dev/null
request '{"jsonrpc":"2.0","id":2101,"method":"resources/subscribe","params":{"uri":"slurm-log://clusters"}}' | grep -F '"result":{}' >/dev/null
request '{"jsonrpc":"2.0","id":211,"method":"resources/subscribe","params":{"uri":"slurm-log://clusters/alpha/jobs"}}' | grep -F '"result":{}' >/dev/null
request '{"jsonrpc":"2.0","id":212,"method":"resources/subscribe","params":{"uri":"slurm-log://jobs/alpha/123"}}' | grep -F '"result":{}' >/dev/null
request '{"jsonrpc":"2.0","id":213,"method":"resources/subscribe","params":{"uri":"slurm-log://jobs/alpha/123/details"}}' | grep -F '"result":{}' >/dev/null
sleep 1
request '{"jsonrpc":"2.0","id":214,"method":"resources/unsubscribe","params":{"uri":"slurm-log://clusters"}}' | grep -F '"result":{}' >/dev/null
request '{"jsonrpc":"2.0","id":215,"method":"resources/unsubscribe","params":{"uri":"slurm-log://clusters/alpha/jobs"}}' | grep -F '"result":{}' >/dev/null
request '{"jsonrpc":"2.0","id":216,"method":"resources/unsubscribe","params":{"uri":"slurm-log://jobs/alpha/123"}}' | grep -F '"result":{}' >/dev/null
request '{"jsonrpc":"2.0","id":217,"method":"resources/unsubscribe","params":{"uri":"slurm-log://jobs/alpha/123/details"}}' | grep -F '"result":{}' >/dev/null
request '{"jsonrpc":"2.0","id":218,"method":"resources/subscribe","params":{"uri":"slurm-log://jobs/alpha/999"}}' | grep -F 'owned by the configured' >/dev/null
request '{"jsonrpc":"2.0","id":22,"method":"resources/subscribe","params":{"uri":"slurm-log://jobs/alpha/123/log"}}' | grep -F '"result":{}' >/dev/null
sleep 1
printf 'subscription append\n' >>"$log_file"
(receive >"$test_root/notification") & notification_reader=$!
attempt=0
while kill -0 "$notification_reader" 2>/dev/null; do
    attempt=$((attempt + 1))
    if test "$attempt" -ge 120; then kill "$notification_reader"; exit 1; fi
    sleep 0.1
done
wait "$notification_reader"
grep -F '"method":"notifications/resources/updated"' "$test_root/notification" >/dev/null
request '{"jsonrpc":"2.0","id":23,"method":"resources/unsubscribe","params":{"uri":"slurm-log://jobs/alpha/123/log"}}' | grep -F '"result":{}' >/dev/null

exec 3>&-
wait "$server_pid"
test ! -s "$test_root/mcp.err"
awk 'substr($0,1,1) != "{" { exit 1 }' "$transcript"

# Doctor uses the same fake scheduler/daemon boundary as the MCP server.
mkdir -p "$test_root/doctor-state"
sed "s|$state_dir/state.json|$test_root/doctor-state/state.json|" \
    "$test_root/config.json" >"$test_root/doctor-config.json"
SLURM_LOG_CONFIG="$test_root/doctor-config.json" "$binary" mcp doctor |
    grep -F 'private daemon: ok' >/dev/null

# Forty stdio clients share the private daemon scheduler snapshot.
shared_state=$test_root/shared-state
mkdir -p "$shared_state"
sed "s|$state_dir/state.json|$shared_state/state.json|" "$test_root/config.json" >"$test_root/shared-config.json"
: >"$MCP_CALLS"
cat >"$test_root/one-client.in" <<'EOF'
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"shared","version":"1"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"slurm_list_jobs","arguments":{"cluster":"alpha"}}}
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"slurm_read_log","arguments":{"cluster":"alpha","job_id":"123","filter":"all"}}}
EOF
pids=
index=1
while test "$index" -le 40; do
    SLURM_LOG_CONFIG="$test_root/shared-config.json" "$binary" mcp \
        <"$test_root/one-client.in" >"$test_root/client-$index.out" &
    pids="$pids $!"
    index=$((index + 1))
done
for pid in $pids; do wait "$pid"; done
test "$(grep '^squeue ' "$MCP_CALLS" | grep -vc ' -j ')" -eq 1
test "$(grep -c '^squeue .* -j 123 ' "$MCP_CALLS")" -ge 40
test "$(grep -c '^scontrol .*show job.*123' "$MCP_CALLS")" -ge 40

printf 'mcp_server: ok (stdio, tools, resources, logs, mutations, subscriptions, sharing; fully offline)\n'
