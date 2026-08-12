#!/bin/sh
# Offline PTY regression for SSH alias selection and automatic remote defaults.
# `ssh` is a local fixture; this test never opens a network connection.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=${SLURM_LOG_TEST_BINARY:-$project_dir/target/release/slurm-log}
test_root=$(mktemp -d)
case "$test_root" in /tmp/*) ;; *) exit 1 ;; esac
trap 'rm -rf "$test_root"' EXIT HUP INT TERM

home_dir=$test_root/home
fake_bin=$test_root/bin
config=$test_root/config.json
calls=$test_root/ssh-calls
mkdir -p "$home_dir/.ssh" "$fake_bin"
cat >"$home_dir/.ssh/config" <<'EOF'
Host alpha
  HostName alpha.invalid
Host beta
  HostName beta.invalid
EOF
cat >"$fake_bin/ssh" <<'EOF'
#!/bin/sh
printf 'CALL\n%s\n' "$*" >>"$SETUP_SSH_CALLS"
test "${SETUP_SSH_FAIL:-0}" = 0 || exit 7
printf 'SLURM_LOG_USER=remote-user\n'
printf 'SLURM_LOG_HOME=/remote/home/remote-user\n'
printf 'SLURM_LOG_CLUSTER=gpu cluster\n'
printf 'SLURM_LOG_ACCOUNTING=yes\n'
EOF
chmod 755 "$fake_bin/ssh"

# Keep the default cluster count, choose SSH, move from alpha to beta, accept
# all detected defaults, skip bank discovery, and add no manual bank.
transcript=$test_root/transcript
if ! (
    # Delay picker input until setup has switched the PTY from canonical to raw
    # mode; otherwise util-linux `script` may deliver it to the prior prompt.
    printf '\nssh\n'
    sleep 0.2
    # A physical Enter key sends carriage return in raw terminal mode.
    printf 'j\r'
    sleep 0.2
    printf '\n\n\n\nno\nno\n\n'
) | env \
        HOME="$home_dir" USER=local-user \
        SLURM_LOG_CONFIG="$config" SETUP_SSH_CALLS="$calls" \
        PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" \
        timeout 15 script -qefc "$binary setup" /dev/null >"$transcript"; then
    sed -n '1,160p' "$transcript" >&2
    exit 1
fi

grep -q '"name": "gpu-cluster"' "$config"
grep -q '"transport": "ssh"' "$config"
grep -q '"user": "remote-user"' "$config"
grep -q '"sshHost": "beta"' "$config"
grep -q '"workingDirectory": "/remote/home/remote-user"' "$config"
grep -q '"accounting": true' "$config"
test "$(grep -c '^CALL$' "$calls")" -eq 1
grep -q 'ControlMaster=auto' "$calls"

# Exercise local configuration, subprocess bank discovery, candidate
# selection, and the manual folder browser. All paths stay under /tmp.
local_home=$test_root/local-home
scan_root=$test_root/workspaces
repository=$scan_root/project
local_config=$test_root/local-config.json
mkdir -p "$local_home" "$repository/.git" "$repository/jobs"
printf '#!/bin/sh\n#SBATCH --job-name=offline\n' >"$repository/jobs/train.sbatch"
local_transcript=$test_root/local-transcript
if ! (
    # One local cluster and all default cluster fields.
    printf '\n\n\n\n\n\n'
    # Discover only the controlled workspace and accept its repository.
    printf 'yes\n%s\n\n' "$scan_root"
    # Add one manual bank with the raw-mode browser. The first root is the
    # current repository; j selects the next root (our temporary HOME).
    printf 'yes\nyes\n'
    sleep 0.2
    printf 'j\r'
    sleep 0.1
    printf '\r'
    sleep 0.2
    printf 'Manual Home\nno\n\n'
) | env \
        HOME="$local_home" USER=local-user \
        SLURM_LOG_CONFIG="$local_config" \
        PATH="/usr/local/bin:/usr/bin:/bin" \
        timeout 15 script -qefc "$binary setup" /dev/null >"$local_transcript"; then
    sed -n '1,220p' "$local_transcript" >&2
    exit 1
fi
grep -q '"name": "local"' "$local_config"
grep -F "\"path\": \"$repository\"" "$local_config" >/dev/null
grep -F '"name": "Manual Home"' "$local_config" >/dev/null
grep -F 'Scanning locally' "$local_transcript" >/dev/null
grep -F 'Choose an sbatch bank directory' "$local_transcript" >/dev/null

# Re-running setup preserves explicit cluster and bank defaults. This covers
# the update workflow rather than only first-time configuration.
reuse_transcript=$test_root/reuse-transcript
printf '\n\n\n\n\n\nno\n\nno\n\n' | env \
    HOME="$local_home" USER=local-user SLURM_LOG_CONFIG="$local_config" \
    PATH="/usr/local/bin:/usr/bin:/bin" \
    timeout 15 script -qefc "$binary setup" /dev/null >"$reuse_transcript"
grep -F 'Existing cluster configuration found' "$reuse_transcript" >/dev/null
grep -F "$repository" "$reuse_transcript" >/dev/null
grep -F '"name": "local"' "$local_config" >/dev/null

# Invalid interactive inputs fail clearly and never fall through to later
# setup stages. Each case uses its own private config and a real PTY.
invalid_count=$test_root/invalid-count
if printf '0\n' | env HOME="$local_home" USER=local-user \
    SLURM_LOG_CONFIG="$test_root/invalid-count.json" \
    PATH="/usr/local/bin:/usr/bin:/bin" \
    timeout 10 script -qefc "$binary setup" /dev/null >"$invalid_count" 2>&1; then
    exit 1
fi
grep -F 'configure between 1 and 16 clusters' "$invalid_count" >/dev/null

invalid_transport=$test_root/invalid-transport
if printf '\ninvalid\n' | env HOME="$local_home" USER=local-user \
    SLURM_LOG_CONFIG="$test_root/invalid-transport.json" \
    PATH="/usr/local/bin:/usr/bin:/bin" \
    timeout 10 script -qefc "$binary setup" /dev/null >"$invalid_transport" 2>&1; then
    exit 1
fi
grep -F 'cluster connection must be local or ssh' "$invalid_transport" >/dev/null

invalid_roots=$test_root/invalid-roots
if printf "\n\n\n\n\n\nyes\n'\n" | env HOME="$local_home" USER=local-user \
    SLURM_LOG_CONFIG="$test_root/invalid-roots.json" \
    PATH="/usr/local/bin:/usr/bin:/bin" \
    timeout 10 script -qefc "$binary setup" /dev/null >"$invalid_roots" 2>&1; then
    exit 1
fi
grep -F 'parse workspace roots' "$invalid_roots" >/dev/null

empty_bank=$test_root/empty-bank
if printf '\n\n\n\n\n\nno\nyes\nno\n\n' | env HOME="$local_home" USER=local-user \
    SLURM_LOG_CONFIG="$test_root/empty-bank.json" \
    PATH="/usr/local/bin:/usr/bin:/bin" \
    timeout 10 script -qefc "$binary setup" /dev/null >"$empty_bank" 2>&1; then
    exit 1
fi
grep -F 'sbatch bank directory must not be empty' "$empty_bank" >/dev/null

# Navigate into a child directory rather than selecting only a suggested root,
# then cover explicit browser cancellation in a separate setup run.
browser_home=$test_root/browser-home
browser_child=$browser_home/nested-bank
browser_config=$test_root/browser-config.json
mkdir -p "$browser_child"
(
    printf '\n\n\n\n\n\nno\nyes\nyes\n'
    sleep 0.2
    printf 'j\r'
    sleep 0.1
    printf 'jj\r'
    sleep 0.1
    printf '\r'
    sleep 0.2
    printf '\nno\n\n'
) | env HOME="$browser_home" USER=local-user SLURM_LOG_CONFIG="$browser_config" \
    PATH="/usr/local/bin:/usr/bin:/bin" \
    timeout 15 script -qefc "$binary setup" /dev/null >"$test_root/browser.out"
grep -F "\"path\": \"$browser_child\"" "$browser_config" >/dev/null

cancel_config=$test_root/cancel-config.json
(
    printf '\n\n\n\n\n\nno\nyes\nyes\n'
    sleep 0.2
    printf 'q'
    sleep 0.2
    printf 'no\n\n'
) | env HOME="$browser_home" USER=local-user SLURM_LOG_CONFIG="$cancel_config" \
    PATH="/usr/local/bin:/usr/bin:/bin" \
    timeout 15 script -qefc "$binary setup" /dev/null >"$test_root/cancel.out"
grep -F 'Folder selection cancelled' "$test_root/cancel.out" >/dev/null

# With no SSH aliases, setup accepts a manually typed literal host and rejects
# option-like hosts before any SSH process can be started.
manual_ssh_config=$test_root/manual-ssh.json
printf '\nssh\nmanual.invalid\n\n\n\n\nno\nno\n\n' | env \
    HOME="$browser_home" USER=local-user SLURM_LOG_CONFIG="$manual_ssh_config" \
    SETUP_SSH_CALLS="$calls" PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" \
    timeout 15 script -qefc "$binary setup" /dev/null >"$test_root/manual-ssh.out"
grep -F '"sshHost": "manual.invalid"' "$manual_ssh_config" >/dev/null

# A failed offline SSH probe falls back to editable defaults and still writes
# a valid configuration when the user accepts them.
failed_probe_config=$test_root/failed-probe.json
printf '\nssh\nunreachable.invalid\n\n\n\n\nno\nno\n\n' | env \
    HOME="$browser_home" USER=local-user SLURM_LOG_CONFIG="$failed_probe_config" \
    SETUP_SSH_CALLS="$calls" SETUP_SSH_FAIL=1 \
    PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" \
    timeout 15 script -qefc "$binary setup" /dev/null >"$test_root/failed-probe.out"
grep -F 'Could not probe unreachable.invalid' "$test_root/failed-probe.out" >/dev/null
grep -F '"name": "unreachable.invalid"' "$failed_probe_config" >/dev/null

# Manual entry remains available without the folder browser.
manual_bank_config=$test_root/manual-bank.json
printf '\n\n\n\n\n\nno\nyes\nno\n%s\nExplicit Bank\nno\n\n' "$browser_child" | env \
    HOME="$browser_home" USER=local-user SLURM_LOG_CONFIG="$manual_bank_config" \
    PATH="/usr/local/bin:/usr/bin:/bin" \
    timeout 15 script -qefc "$binary setup" /dev/null >"$test_root/manual-bank.out"
grep -F "\"path\": \"$browser_child\"" "$manual_bank_config" >/dev/null
grep -F '"name": "Explicit Bank"' "$manual_bank_config" >/dev/null

invalid_yes_no=$test_root/invalid-yes-no.out
if printf '\n\n\n\n\nmaybe\n' | env HOME="$browser_home" USER=local-user \
    SLURM_LOG_CONFIG="$test_root/invalid-yes-no.json" \
    PATH="/usr/local/bin:/usr/bin:/bin" \
    timeout 10 script -qefc "$binary setup" /dev/null >"$invalid_yes_no" 2>&1; then
    exit 1
fi
grep -F 'answer yes or no' "$invalid_yes_no" >/dev/null

invalid_host=$test_root/invalid-host.out
if printf '\nssh\n-bad\n' | env HOME="$browser_home" USER=local-user \
    SLURM_LOG_CONFIG="$test_root/invalid-host.json" SETUP_SSH_CALLS="$calls" \
    PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" \
    timeout 10 script -qefc "$binary setup" /dev/null >"$invalid_host" 2>&1; then
    exit 1
fi
grep -F "must not begin with '-'" "$invalid_host" >/dev/null

# Setup is deliberately rejected without a controlling terminal.
if env HOME="$local_home" USER=local-user SLURM_LOG_CONFIG="$test_root/no-tty.json" \
    PATH="/usr/local/bin:/usr/bin:/bin" "$binary" setup </dev/null >/dev/null 2>"$test_root/no-tty.err"; then
    exit 1
fi
grep -F 'setup requires an interactive terminal' "$test_root/no-tty.err" >/dev/null

printf 'setup_wizard: ok (SSH/local setup + discovery/browser; fully offline)\n'
