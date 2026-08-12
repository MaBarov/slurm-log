#!/bin/sh
# Exhaustive offline regression for the public CLI surface. All scheduler,
# SSH, fzf, and tmux commands resolve to deterministic local fixtures.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=${SLURM_LOG_TEST_BINARY:-$project_dir/target/release/slurm-log}
test_root=$(mktemp -d)
case "$test_root" in /tmp/*) ;; *) exit 1 ;; esac
fake_bin=$test_root/bin
home_dir=$test_root/home
state=$test_root/state/state.json
config=$test_root/config.json
calls=$test_root/calls
mkdir -p "$fake_bin" "$home_dir" "$test_root/state" "$test_root/bank/extra"
mkdir -p "$test_root/bank-duplicate"

cleanup() {
    env PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" HOME="$home_dir" \
        SLURM_LOG_CONFIG="$config" "$binary" daemon stop >/dev/null 2>&1 || true
    rm -rf "$test_root"
}
trap cleanup EXIT HUP INT TERM

now=$(date --iso-8601=seconds)
printf 'first log line\nWARNING expected warning\nRuntimeError: expected exception\n' >"$test_root/job-101.log"
cat >"$test_root/bank/train.sbatch" <<'EOF'
#!/bin/sh
#SBATCH --job-name=bank-train
EOF
cat >"$test_root/bank/extra/eval.sbatch" <<'EOF'
#!/bin/sh
#SBATCH --job-name=bank-eval
EOF
cp "$test_root/bank/train.sbatch" "$test_root/bank-duplicate/train.sbatch"

cat >"$fake_bin/squeue" <<'EOF'
#!/bin/sh
printf 'squeue %s\n' "$*" >>"$CLI_CALL_LOG"
printf '101|RUNNING|alpha-run|00:01|alpha-node|cpu|2026-08-12T10:00:00|1000|train.sbatch\n'
printf '102|PENDING|alpha-blocked|00:00|DependencyNeverSatisfied|cpu|Unknown|10|blocked.sbatch\n'
printf '103|PENDING|alpha-wait|00:00|Resources|gpu|Unknown|20|wait.sbatch\n'
printf '104|RUNNING|alpha-shell|00:02|alpha-node|cpu|2026-08-12T10:00:00|30|bash\n'
EOF
cat >"$fake_bin/sacct" <<'EOF'
#!/bin/sh
printf 'sacct %s\n' "$*" >>"$CLI_CALL_LOG"
case " $* " in *' -j 99999999 '*) exit 0 ;; esac
printf '301|COMPLETED|alpha-complete|00:03|%s|0:0|1G|cpu=2,mem=2G|cpu\n' "$CLI_NOW"
printf '302|FAILED|alpha-failed|00:04|%s|1:0|2G|cpu=2,mem=2G|cpu\n' "$CLI_NOW"
EOF
cat >"$fake_bin/scontrol" <<'EOF'
#!/bin/sh
printf 'scontrol %s\n' "$*" >>"$CLI_CALL_LOG"
case "$*" in
    'show job 101')
        printf 'JobId=101 JobName=alpha-run JobState=RUNNING StdOut=%s/job-101.log\n' "$CLI_ROOT"
        ;;
    'show job -o 101')
        printf 'JobId=101 JobName=alpha-run JobState=RUNNING Reason=None RunTime=00:01:00 TimeLimit=01:00:00 NumNodes=1 NumCPUs=4 Partition=cpu NodeList=alpha-node Account=test QOS=normal ReqTRES=cpu=4,mem=8G AllocTRES=cpu=4,mem=8G ExitCode=0:0\n'
        ;;
    *) exit 1 ;;
esac
EOF
cat >"$fake_bin/sstat" <<'EOF'
#!/bin/sh
printf 'sstat %s\n' "$*" >>"$CLI_CALL_LOG"
printf '101.batch|4|cpu=4,mem=8G|00:02:00|1G|512M||\n'
EOF
cat >"$fake_bin/ssh" <<'EOF'
#!/bin/sh
printf 'ssh %s\n' "$*" >>"$CLI_CALL_LOG"
case "$*" in
    *'squeue -h'*)
        printf '201|RUNNING|beta-run|00:02|beta-node|gpu|2026-08-12T10:00:00|2000|remote.sbatch\n'
        ;;
    *'sacct -X -S'*)
        printf '401|COMPLETED|beta-complete|00:05|%s|0:0|3G|cpu=4,gres/gpu=1|gpu\n' "$CLI_NOW"
        ;;
    *) exit 23 ;;
esac
EOF
cat >"$fake_bin/fzf" <<'EOF'
#!/bin/sh
printf 'fzf %s\n' "$*" >>"$CLI_CALL_LOG"
first=
while IFS= read -r line; do
    test -n "$first" || first=$line
done
test -z "$first" || printf '%s\n' "$first"
EOF
cat >"$fake_bin/tmux" <<'EOF'
#!/bin/sh
printf 'tmux %s\n' "$*" >>"$CLI_CALL_LOG"
case "${1:-}" in
    display-message) printf '%%77\n' ;;
    list-sessions) printf 'ordinary\nslurm-logs-offline-one\nslurm-logs-offline-two\n' ;;
esac
exit 0
EOF
cat >"$fake_bin/scancel" <<'EOF'
#!/bin/sh
printf 'scancel %s\n' "$*" >>"$CLI_CALL_LOG"
test "${CLI_SCANCEL_FAIL:-0}" = 0
EOF
cat >"$fake_bin/sbatch" <<'EOF'
#!/bin/sh
printf 'sbatch %s\n' "$*" >>"$CLI_CALL_LOG"
cat >"$CLI_ROOT/submitted-input"
printf '777\n'
EOF
chmod 755 "$fake_bin"/*

cat >"$config" <<EOF
{
  "clusters": [
    {"name":"alpha","transport":"local","user":"offline-alpha","workingDirectory":"$test_root","accounting":true},
    {"name":"beta","transport":"ssh","user":"offline-beta","sshHost":"beta.invalid","workingDirectory":"/offline","accounting":true}
  ],
  "sbatchBanks": [
    {"path":"$test_root/bank","name":"Fixtures"},
    {"path":"$test_root/bank-duplicate","name":"Duplicate"}
  ],
  "statePath":"$state"
}
EOF

export PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin"
export HOME=$home_dir
export XDG_CONFIG_HOME=$home_dir/config
export XDG_STATE_HOME=$home_dir/state
export SLURM_LOG_CONFIG=$config
export SLURM_LOG_STATE=$state
export SLURM_LOG_ARCHIVE_DAYS=7
export CLI_CALL_LOG=$calls
export CLI_NOW=$now
export CLI_ROOT=$test_root

expect_fail() {
    if "$binary" "$@" >"$test_root/fail.out" 2>"$test_root/fail.err"; then
        printf 'Expected failure: slurm-log %s\n' "$*" >&2
        exit 1
    fi
    test -s "$test_root/fail.err"
}

# Help/version and every parser-level validation path.
"$binary" --help >"$test_root/help-long"
"$binary" -h >"$test_root/help-short"
cmp "$test_root/help-long" "$test_root/help-short"
grep -F 'slurm-log — fast, owner-scoped' "$test_root/help-long" >/dev/null
test "$("$binary" --version)" = 'slurm-log 0.1.4'
test "$("$binary" -V)" = 'slurm-log 0.1.4'
expect_fail --does-not-exist
expect_fail --cluster
expect_fail --lines nope
expect_fail --refresh 0
expect_fail --binary
expect_fail all --binary "$binary"
expect_fail all --purge
expect_fail update unexpected
expect_fail uninstall unexpected
expect_fail all --cluster ../escape
expect_fail nonexistent-mode
expect_fail details invalid --cluster alpha
expect_fail read 999999
expect_fail submit train.sbatch
expect_fail cancel 101
expect_fail cancel --cluster alpha
expect_fail cancel invalid --cluster alpha
expect_fail daemon nonsense
expect_fail toggle-auto
expect_fail auto-monitor

# Default and named views, including non-TTY rendering and accounting archive.
"$binary" --me --cluster all >"$test_root/all"
grep -F alpha-run "$test_root/all" >/dev/null
grep -F beta-run "$test_root/all" >/dev/null
grep -F alpha-wait "$test_root/all" >/dev/null
! grep -F alpha-blocked "$test_root/all" >/dev/null
! grep -F alpha-shell "$test_root/all" >/dev/null
"$binary" running --cluster all >"$test_root/running"
grep -F alpha-run "$test_root/running" >/dev/null
grep -F beta-run "$test_root/running" >/dev/null
! grep -F alpha-wait "$test_root/running" >/dev/null
"$binary" failed --cluster alpha >"$test_root/failed"
grep -F alpha-failed "$test_root/failed" >/dev/null
! grep -F alpha-run "$test_root/failed" >/dev/null
"$binary" blocked --cluster alpha >"$test_root/blocked"
grep -F alpha-blocked "$test_root/blocked" >/dev/null
grep -F alpha-shell "$test_root/blocked" >/dev/null
"$binary" archive --cluster all >"$test_root/archive"
grep -F alpha-complete "$test_root/archive" >/dev/null
grep -F beta-complete "$test_root/archive" >/dev/null
grep -F 'sacct -X -S' "$calls" | grep -F -- '-u offline-alpha' >/dev/null

# JSON, state read/unread, and explicit active-monitor suppression.
"$binary" json --cluster all >"$test_root/jobs.json"
grep -F '"cluster": "alpha"' "$test_root/jobs.json" >/dev/null
grep -F '"id": "101"' "$test_root/jobs.json" >/dev/null
"$binary" read 101 | grep -F 'Marked job 101 read' >/dev/null
grep -F '"alpha:101"' "$state" >/dev/null
"$binary" unread 101 | grep -F 'Marked job 101 unread' >/dev/null
"$binary" suppress 101 --cluster alpha
grep -F '"dismissed":{"alpha:101"' "$state" >/dev/null
expect_fail suppress --cluster alpha
expect_fail suppress invalid --cluster alpha
expect_fail suppress 101

# Details, configured/temporary banks, submit, and cancellation.
"$binary" details 101 --cluster alpha >"$test_root/details"
grep -F 'Job: alpha:101 alpha-run' "$test_root/details" >/dev/null
grep -F 'Allocation: 1 nodes, 4 CPUs' "$test_root/details" >/dev/null
SLURM_LOG_DETAILS_PANE=1 TMUX_PANE=%55 "$binary" details 101 --cluster alpha \
    >"$test_root/details-pane"
grep -F 'tmux kill-pane -t %55' "$calls" >/dev/null
"$binary" bank >"$test_root/bank-list"
grep -F 'Fixtures/train.sbatch' "$test_root/bank-list" >/dev/null
"$binary" bank --bank-dir "$test_root/bank/extra" >"$test_root/temporary-bank"
grep -F 'eval.sbatch' "$test_root/temporary-bank" >/dev/null
SLURM_LOG_SBATCH_BANK="$test_root/bank/extra" "$binary" bank >"$test_root/environment-bank"
grep -F 'eval.sbatch' "$test_root/environment-bank" >/dev/null
"$binary" submit Fixtures/train.sbatch --cluster alpha | grep -F 'alpha:777' >/dev/null
cmp "$test_root/bank/train.sbatch" "$test_root/submitted-input"
"$binary" cancel 101 103 --cluster alpha | grep -F '2 job(s)' >/dev/null
grep -F 'scancel 101 103' "$calls" >/dev/null
CLI_SCANCEL_FAIL=1 expect_fail cancel 101 --cluster alpha
expect_fail submit missing.sbatch --cluster alpha

# An unqualified path shared by multiple named banks is rejected so the user
# cannot accidentally submit the wrong script.
expect_fail submit train.sbatch --cluster alpha

# Direct open forms, selection through fzf, newest-job mode, and watcher args.
: >"$calls"
"$binary" alpha 101 --lines 7 --show-log-warnings
grep -F 'tmux new-session' "$calls" >/dev/null
grep -F 'tmux respawn-pane -k -t %77' "$calls" | grep -F -- '--pane-follow --lines 7' | grep -F -- '--show-log-warnings alpha 101' >/dev/null
: >"$calls"
"$binary" 101 --lines 9
grep -F 'scontrol show job 101' "$calls" >/dev/null
grep -F 'tmux respawn-pane -k -t %77' "$calls" | grep -F -- '--lines 9' >/dev/null
expect_fail 99999999
: >"$calls"
"$binary" fzf --cluster alpha
grep -F 'fzf -m' "$calls" >/dev/null
grep -F 'tmux new-session' "$calls" >/dev/null
: >"$calls"
script -qec "$binary last --cluster alpha" /dev/null >/dev/null
grep -F 'tmux new-session' "$calls" >/dev/null

# Terminal following preserves exceptions, filters warnings by default, and
# includes warnings only when explicitly requested.
timeout -s INT -k 1 0.3 "$binary" alpha 101 --follow --lines 20 >"$test_root/follow-default" 2>&1 || true
grep -F 'RuntimeError: expected exception' "$test_root/follow-default" >/dev/null
! grep -F 'WARNING expected warning' "$test_root/follow-default" >/dev/null
timeout -s INT -k 1 0.3 "$binary" alpha 101 --follow --lines 20 --show-log-warnings >"$test_root/follow-warnings" 2>&1 || true
grep -F 'WARNING expected warning' "$test_root/follow-warnings" >/dev/null

# Watch renders immediately before sleeping; bound it locally with timeout.
timeout -s TERM -k 1 0.3 "$binary" watch --cluster alpha --refresh 1 >"$test_root/watch" 2>&1 || true
grep -F 'CLUSTER' "$test_root/watch" >/dev/null

# Session commands route only slurm-log workspaces. Attach is tested in tmux
# mode so the command uses switch-client without needing a real client here.
"$binary" sessions >"$test_root/sessions"
grep -Fx slurm-logs-offline-one "$test_root/sessions" >/dev/null
grep -Fx slurm-logs-offline-two "$test_root/sessions" >/dev/null
! grep -Fx ordinary "$test_root/sessions" >/dev/null
TMUX=offline "$binary" attach slurm-logs-offline-one
"$binary" close slurm-logs-offline-one
"$binary" close all
grep -F 'tmux switch-client -t slurm-logs-offline-one' "$calls" >/dev/null
grep -F 'tmux kill-session -t slurm-logs-offline-one' "$calls" >/dev/null
grep -F 'tmux kill-session -t slurm-logs-offline-two' "$calls" >/dev/null

# User/host/state overrides must reach only their intended scheduler boundary.
alternate=$test_root/alternate/state.json
: >"$calls"
"$binary" json --cluster all --local-user override-local --remote-user override-remote \
    --ssh-host override.invalid --state-path "$alternate" >/dev/null
grep -F 'squeue -h -u override-local' "$calls" >/dev/null
grep -F 'ssh ' "$calls" | grep -F 'override.invalid' | grep -F -- '-u override-remote' >/dev/null
test -f "$alternate"

printf 'cli_surface: ok (all public commands, views, direct modes, options; fully offline)\n'
