#!/bin/sh
# Offline submission/cancellation regression. Every scheduler/SSH command is fake.
set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=${SLURM_LOG_TEST_BINARY:-$project_dir/target/release/slurm-log}
test_root=$(mktemp -d)
case "$test_root" in /tmp/*) ;; *) exit 1 ;; esac
cleanup() { rm -rf "$test_root"; }
trap cleanup EXIT HUP INT TERM

mkdir -p "$test_root/bin" "$test_root/bank/group" "$test_root/second-bank" "$test_root/work" "$test_root/state"
cat >"$test_root/bank/group/train.sbatch" <<'EOF'
#!/bin/sh
#SBATCH --job-name=offline-train
#SBATCH --gpus=2
printf never-executed
EOF
cat >"$test_root/second-bank/eval.sbatch" <<'EOF'
#!/bin/sh
#SBATCH --job-name=eval
EOF
cat >"$test_root/bin/sbatch" <<'EOF'
#!/bin/sh
printf '%s\n' "$PWD" >"$BANK_CALLS/local-pwd"
cat >"$BANK_CALLS/local-input"
printf '12345\n'
EOF
cat >"$test_root/bin/scancel" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >"$BANK_CALLS/local-cancel"
EOF
cat >"$test_root/bin/squeue" <<'EOF'
#!/bin/sh
case " $* " in
  *' -j 12345 '*) printf '12345|RUNNING|offline-train|00:01|node|cpu|now|1|train.sbatch|offline\n' ;;
  *' -j 12346 '*) printf '12346|RUNNING|offline-train|00:01|node|cpu|now|1|train.sbatch|offline\n' ;;
  *) exit 71 ;;
esac
EOF
cat >"$test_root/bin/scontrol" <<'EOF'
#!/bin/sh
case " $* " in
  *' show job -o 12345 '*) printf 'JobId=12345 UserId=offline(1000) JobName=offline-train JobState=RUNNING\n' ;;
  *' show job -o 12346 '*) printf 'JobId=12346 UserId=offline(1000) JobName=offline-train JobState=RUNNING\n' ;;
  *) exit 72 ;;
esac
EOF
cat >"$test_root/bin/ssh" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"$BANK_CALLS/ssh-args"
case "$*" in
  *'sbatch --parsable'*) cat >"$BANK_CALLS/remote-input"; printf '54321;remote\n' ;;
  *'squeue '*) printf '54321|RUNNING|offline-train|00:01|node|cpu|now|1|train.sbatch|offline\n' ;;
  *'scontrol '*) printf 'JobId=54321 UserId=offline(1000) JobName=offline-train JobState=RUNNING\n' ;;
  *scancel*) : ;;
  *) exit 71 ;;
esac
EOF
chmod 755 "$test_root/bin/sbatch" "$test_root/bin/scancel" "$test_root/bin/squeue" "$test_root/bin/scontrol" "$test_root/bin/ssh"
cat >"$test_root/config.json" <<EOF
{
  "clusters": [
    {"name":"local","transport":"local","user":"offline","workingDirectory":"$test_root/work"},
    {"name":"remote","transport":"ssh","user":"offline","sshHost":"offline.invalid","workingDirectory":"/remote/work"}
  ],
  "sbatchBanks": [
    {"path":"$test_root/bank","name":"Training"},
    {"path":"$test_root/second-bank","name":"Evaluation"}
  ],
  "statePath": "$test_root/state/state.json"
}
EOF
chmod 600 "$test_root/config.json"
common_env="PATH=$test_root/bin:/usr/local/bin:/usr/bin:/bin SLURM_LOG_CONFIG=$test_root/config.json BANK_CALLS=$test_root"
env $common_env "$binary" submit group/train.sbatch --cluster local | grep -q 'local:12345'
env $common_env "$binary" bank | grep -q 'Training/group/train.sbatch'
env $common_env "$binary" bank | grep -q 'Evaluation/eval.sbatch'
test "$(cat "$test_root/local-pwd")" = "$test_root/work"
cmp "$test_root/bank/group/train.sbatch" "$test_root/local-input"
env $common_env "$binary" submit group/train.sbatch --cluster remote | grep -q 'remote:54321'
cmp "$test_root/bank/group/train.sbatch" "$test_root/remote-input"
grep -q 'cd /remote/work && exec sbatch --parsable' "$test_root/ssh-args"
env $common_env "$binary" cancel 12345 12346 --cluster local >/dev/null
test "$(cat "$test_root/local-cancel")" = "12345 12346"
env $common_env "$binary" cancel 54321 --cluster remote >/dev/null
grep -q 'scancel.*54321' "$test_root/ssh-args"
printf 'bank_actions: ok (exact stdin, local/SSH routing, cancellation; fully offline)\n'
