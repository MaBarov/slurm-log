#!/bin/sh
# PTY regression for the piped installer (curl | bash) run from a terminal.
#
# The installer must keep reading the script from the pipe while the setup
# wizard and the PATH prompt read from /dev/tty directly. Redirecting fd 0
# (exec </dev/tty) makes bash consume the rest of the script from the
# terminal, which stalls the install silently before anything is printed.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=${SLURM_LOG_TEST_BINARY:-$project_dir/target/release/slurm-log}
test_root=$(mktemp -d)
case "$test_root" in /tmp/*) ;; *) exit 1 ;; esac
trap 'rm -rf "$test_root"' EXIT HUP INT TERM

test_private_key=$test_root/release-private.pem
test_public_pem=$test_root/release-public.pem
umask 077
openssl genpkey -algorithm ED25519 -out "$test_private_key" >/dev/null 2>&1
openssl pkey -in "$test_private_key" -pubout -out "$test_public_pem" >/dev/null 2>&1
test_public_key=$(openssl pkey -pubin -in "$test_public_pem" -pubout -outform DER | \
    tail -c 32 | od -An -tx1 | tr -d ' \n')
[ "${#test_public_key}" -eq 64 ]

fake_bin=$test_root/fake-bin
mkdir -p "$fake_bin"
# package.sh always invokes Cargo to prevent stale release artifacts; the
# hermetic test binary from test-all.sh replaces the real packaging build.
printf '#!/bin/sh\nexit 0\n' >"$fake_bin/cargo"
chmod 755 "$fake_bin/cargo"

release_fixture=$test_root/release
mkdir -p "$release_fixture"
release_archive=$release_fixture/slurm-log-linux-x86_64.tar.gz
SLURM_LOG_TEST_BUILD=1 \
SLURM_LOG_TEST_RELEASE_PUBLIC_KEY="$test_public_key" \
SLURM_LOG_TARGET=x86_64-unknown-linux-musl \
SLURM_LOG_ALLOW_PACKAGE_BINARY=1 \
SLURM_LOG_PACKAGE_BINARY=$binary \
    PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" \
    "$project_dir/package.sh" "$release_archive" >/dev/null
openssl pkeyutl -sign -inkey "$test_private_key" -rawin \
    -in "$release_archive.manifest" -out "$release_archive.manifest.sig"
test -s "$release_archive.sha256"
test -s "$release_archive.manifest"

cat >"$fake_bin/curl" <<'EOF'
#!/bin/sh
output=
url=
while [ "$#" -gt 0 ]; do
    case "$1" in
        -o|--output) output=$2; shift 2 ;;
        --retry|--connect-timeout|--max-time|--max-filesize|--proto) shift 2 ;;
        -fsSL) shift ;;
        *) url=$1; shift ;;
    esac
done
if [ "$output" = - ]; then
    cat "$SLURM_LOG_RELEASE_FIXTURE/$(basename -- "$url")"
else
    cp "$SLURM_LOG_RELEASE_FIXTURE/$(basename -- "$url")" "$output"
fi
EOF
chmod 755 "$fake_bin/curl"

standalone=$test_root/standalone
mkdir -p "$standalone"
cp "$project_dir/install.sh" "$standalone/install.sh"

# Pipe the installer into bash inside a PTY, exactly like `curl ... | bash`
# from an interactive terminal. Local cluster, decline discovery and manual
# banks, decline the ~/.bashrc update.
pipe_home=$test_root/home
mkdir -p "$pipe_home"
( sleep 1
  printf '\n\n\n\n\n\n'
  sleep 0.3
  printf 'no\nno\n'
  sleep 0.3
  printf '\nn\n' ) | \
SLURM_LOG_RELEASE_FIXTURE=$release_fixture \
SLURM_LOG_RELEASE_ROOT=https://offline.invalid/releases \
HOME=$pipe_home USER=pipe-user TERM=xterm-256color \
XDG_CONFIG_HOME=$pipe_home/.config XDG_STATE_HOME=$pipe_home/.local/state \
PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" \
    timeout 60 script -qefc "cat '$standalone/install.sh' | bash -s -- \
        --release-public-key '$test_public_pem' --prefix '$pipe_home/.local'" \
    /dev/null >"$test_root/transcript" 2>&1

grep -F 'Downloading signed slurm-log latest' "$test_root/transcript" >/dev/null
grep -F 'Starting the setup wizard...' "$test_root/transcript" >/dev/null
grep -F 'Saved' "$test_root/transcript" >/dev/null
grep -F 'Add it to ~/.bashrc now?' "$test_root/transcript" >/dev/null
grep -F 'Installed:' "$test_root/transcript" >/dev/null
if grep -F 'Updated ~/.bashrc' "$test_root/transcript" >/dev/null; then
    printf 'PATH answer n was not honoured\n' >&2
    exit 1
fi
test -s "$pipe_home/.config/slurm-log/config.json"
test -s "$pipe_home/.local/bin/slurm-log"
grep -F '"transport": "local"' "$pipe_home/.config/slurm-log/config.json" >/dev/null
grep -F '"user": "pipe-user"' "$pipe_home/.config/slurm-log/config.json" >/dev/null

# Ctrl-D (EOF) at the PATH prompt must default to yes and still print the
# install summary instead of aborting the script.
eof_home=$test_root/eof-home
mkdir -p "$eof_home"
( sleep 1
  printf '\n\n\n\n\n\n'
  sleep 0.3
  printf 'no\nno\n'
  sleep 0.3
  printf '\n\004\n' ) | \
SLURM_LOG_RELEASE_FIXTURE=$release_fixture \
SLURM_LOG_RELEASE_ROOT=https://offline.invalid/releases \
HOME=$eof_home USER=pipe-user TERM=xterm-256color \
XDG_CONFIG_HOME=$eof_home/.config XDG_STATE_HOME=$eof_home/.local/state \
PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" \
    timeout 60 script -qefc "cat '$standalone/install.sh' | bash -s -- \
        --release-public-key '$test_public_pem' --prefix '$eof_home/.local'" \
    /dev/null >"$test_root/eof-transcript" 2>&1
grep -F 'Installed:' "$test_root/eof-transcript" >/dev/null
grep -F 'Updated ~/.bashrc' "$test_root/eof-transcript" >/dev/null
test -s "$eof_home/.config/slurm-log/config.json"
test -s "$eof_home/.local/bin/slurm-log"

# The same piped flow must work under a POSIX shell (dash), not only bash.
sh_home=$test_root/sh-home
mkdir -p "$sh_home"
( sleep 1
  printf '\n\n\n\n\n\n'
  sleep 0.3
  printf 'no\nno\n'
  sleep 0.3
  printf '\nn\n' ) | \
SLURM_LOG_RELEASE_FIXTURE=$release_fixture \
SLURM_LOG_RELEASE_ROOT=https://offline.invalid/releases \
HOME=$sh_home USER=pipe-user TERM=xterm-256color \
XDG_CONFIG_HOME=$sh_home/.config XDG_STATE_HOME=$sh_home/.local/state \
PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" \
    timeout 60 script -qefc "cat '$standalone/install.sh' | sh -s -- \
        --release-public-key '$test_public_pem' --prefix '$sh_home/.local'" \
    /dev/null >"$test_root/sh-transcript" 2>&1
grep -F 'Starting the setup wizard...' "$test_root/sh-transcript" >/dev/null
grep -F 'Installed:' "$test_root/sh-transcript" >/dev/null
if grep -F 'Updated ~/.bashrc' "$test_root/sh-transcript" >/dev/null; then
    printf 'PATH answer n was not honoured under sh\n' >&2
    exit 1
fi
test -s "$sh_home/.config/slurm-log/config.json"
test -s "$sh_home/.local/bin/slurm-log"

# A failing wizard (invalid input) must not swallow the install summary.
bad_home=$test_root/bad-home
mkdir -p "$bad_home"
( sleep 1
  printf 'x\n'
  sleep 0.3
  printf 'n\n' ) | \
SLURM_LOG_RELEASE_FIXTURE=$release_fixture \
SLURM_LOG_RELEASE_ROOT=https://offline.invalid/releases \
HOME=$bad_home USER=pipe-user TERM=xterm-256color \
XDG_CONFIG_HOME=$bad_home/.config XDG_STATE_HOME=$bad_home/.local/state \
PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin" \
    timeout 60 script -qefc "cat '$standalone/install.sh' | bash -s -- \
        --release-public-key '$test_public_pem' --prefix '$bad_home/.local'" \
    /dev/null >"$test_root/bad-transcript" 2>&1
grep -F 'Setup did not finish' "$test_root/bad-transcript" >/dev/null
grep -F 'Installed:' "$test_root/bad-transcript" >/dev/null
test -s "$bad_home/.local/bin/slurm-log"
# install.sh writes a base owner-scoped configuration; the failed wizard must
# not have replaced it with a cluster configuration.
test -s "$bad_home/.config/slurm-log/config.json"
if grep -F '"clusters"' "$bad_home/.config/slurm-log/config.json" >/dev/null; then
    printf 'Failed wizard saved a cluster configuration\n' >&2
    exit 1
fi

printf 'installer_pipe: ok\n'
