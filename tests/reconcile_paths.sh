#!/bin/sh
# Offline normal-exit coverage for exact tmux pane reconciliation. The picker
# controls a separate private session so removing panes cannot kill the tested
# process before LLVM writes its profile.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=${SLURM_LOG_TEST_BINARY:-$project_dir/target/release/slurm-log}
test_root=$(mktemp -d)
case "$test_root" in /tmp/*) ;; *) exit 1 ;; esac
tmux_root=$test_root/tmux
session=reconcile-target-$$
mkdir -p "$tmux_root" "$test_root/bin" "$test_root/state" "$test_root/home"
chmod 700 "$tmux_root"
cleanup() {
    env TMUX_TMPDIR="$tmux_root" tmux kill-server >/dev/null 2>&1 || true
    rm -rf "$test_root"
}
trap cleanup EXIT HUP INT TERM

cat >"$test_root/bin/squeue" <<'EOF'
#!/bin/sh
for id in 101 102 103; do
    printf '%s|RUNNING|job-%s|00:01|node|cpu|now|%s|sbatch\n' "$id" "$id" "$id"
done
EOF
cat >"$test_root/bin/scontrol" <<EOF
#!/bin/sh
id=\${3:-101}
printf 'JobId=%s JobName=job-%s JobState=RUNNING StdOut=$test_root/job.log\n' "\$id" "\$id"
EOF
chmod 755 "$test_root/bin/squeue" "$test_root/bin/scontrol"
printf 'offline reconcile log\n' >"$test_root/job.log"
config=$test_root/config.json
cat >"$config" <<EOF
{"clusters":[{"name":"alpha","transport":"local","user":"offline","workingDirectory":"$test_root","accounting":false}],"statePath":"$test_root/state/state.json"}
EOF

export TMUX_TMPDIR=$tmux_root
export PATH="$test_root/bin:/usr/local/bin:/usr/bin:/bin"
export HOME=$test_root/home
export SLURM_LOG_CONFIG=$config
export SLURM_LOG_STATE=$test_root/state/state.json

tmux new-session -d -s "$session" sleep 120
anchor=$(tmux display-message -p -t "$session" '#{pane_id}')
tmux set-option -p -t "$anchor" @slurm_log_cluster alpha
tmux set-option -p -t "$anchor" @slurm_log_job_id 101
tmux set-option -p -t "$anchor" @slurm_log_job_name job-101

run_picker() {
    keys=$1
    transcript=$2
    (
        sleep 0.25
        # Interpret \r escapes supplied by each scenario.
        printf '%b' "$keys"
    ) | timeout 15 script -qefc \
        "$binary pick-add $session --cluster alpha --refresh 3600" /dev/null \
        >"$transcript"
}
pane_ids() {
    tmux list-panes -t "$session" -F '#{@slurm_log_job_id}' | sort
}

# Select every visible job: existing pane retained, two panes added and tiled.
run_picker 'v\r' "$test_root/add.out"
test "$(pane_ids | tr '\n' ' ')" = '101 102 103 '

# Keep only 102: obsolete panes are removed before any additions.
run_picker 'c/job-102\r \r' "$test_root/remove.out"
test "$(pane_ids)" = 102

# Replace the complete set: one obsolete anchor remains until 103 exists, then
# it is removed so tmux never has to represent an empty window.
run_picker 'c/job-103\r \r' "$test_root/replace.out"
test "$(pane_ids)" = 103

printf 'reconcile_paths: ok (add/remove/replace; fully offline)\n'
