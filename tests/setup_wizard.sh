#!/bin/sh
# Offline PTY regression for SSH alias selection and automatic remote defaults.
# `ssh` is a local fixture; this test never opens a network connection.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=$project_dir/target/release/slurm-log
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
printf 'setup_wizard: ok (PTY picker + fake SSH probe; fully offline)\n'
