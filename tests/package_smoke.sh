#!/bin/sh
# Offline end-to-end test for installation, configuration security, daemon
# lifecycle, update behavior, uninstall preservation, and package hygiene.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=${SLURM_LOG_TEST_BINARY:-$project_dir/target/release/slurm-log}
test_root=$(mktemp -d)
case "$test_root" in /tmp/*) ;; *) printf 'Unsafe temp path\n' >&2; exit 1 ;; esac
trap 'rm -rf "$test_root"' EXIT HUP INT TERM

fake_bin=$test_root/fake-bin
home_dir=$test_root/home
mkdir -p "$fake_bin" "$home_dir"
for command in tmux ssh squeue scontrol; do
    printf '#!/bin/sh\nexit 0\n' >"$fake_bin/$command"
    chmod 755 "$fake_bin/$command"
done
# package.sh always invokes Cargo to prevent stale release artifacts. The test
# already has the freshly built binary from test-all.sh, so this hermetic stub
# lets it exercise packaging without reaching a toolchain or the network.
printf '#!/bin/sh\nexit 0\n' >"$fake_bin/cargo"
chmod 755 "$fake_bin/cargo"

export HOME=$home_dir
export XDG_CONFIG_HOME=$home_dir/config
export XDG_STATE_HOME=$home_dir/state
export PATH=$fake_bin:/usr/local/bin:/usr/bin:/bin

# A release package carries a matching checksum and can be consumed by a
# standalone copy of install.sh without Cargo or network access.
release_fixture=$test_root/release
release_archive=$release_fixture/slurm-log-linux-x86_64.tar.gz
mkdir -p "$release_fixture"
SLURM_LOG_PACKAGE_BINARY=$binary "$project_dir/package.sh" "$release_archive" >/dev/null
test -s "$release_archive"
test -s "$release_archive.sha256"
expected=$(awk 'NR == 1 { print $1 }' "$release_archive.sha256")
test "$expected" = "$(sha256sum "$release_archive" | awk '{ print $1 }')"
tar -tzf "$release_archive" | grep -Fx 'slurm-log/bin/slurm-log' >/dev/null

cat >"$fake_bin/curl" <<'EOF'
#!/bin/sh
output=
url=
while [ "$#" -gt 0 ]; do
    case "$1" in
        -o) output=$2; shift 2 ;;
        *) url=$1; shift ;;
    esac
done
cp "$SLURM_LOG_RELEASE_FIXTURE/$(basename -- "$url")" "$output"
EOF
chmod 755 "$fake_bin/curl"
standalone=$test_root/standalone
mkdir -p "$standalone"
cp "$project_dir/install.sh" "$standalone/install.sh"
"$standalone/install.sh" --help | grep -F -- '--no-path-update' >/dev/null
if "$standalone/install.sh" --version '../unsafe' >/dev/null 2>&1; then
    printf 'Installer accepted an unsafe release tag\n' >&2
    exit 1
fi
SLURM_LOG_RELEASE_FIXTURE=$release_fixture \
SLURM_LOG_RELEASE_ROOT=https://offline.invalid/releases \
XDG_CONFIG_HOME=$home_dir/download-config \
XDG_STATE_HOME=$home_dir/download-state \
    "$standalone/install.sh" --prefix "$home_dir/download-prefix" \
    --no-setup --no-path-update >/dev/null
cmp "$home_dir/download-prefix/bin/slurm-log" "$binary"

# A forged checksum must fail closed and leave no installed binary.
cp "$release_archive.sha256" "$test_root/good.sha256"
printf '%064d  slurm-log-linux-x86_64.tar.gz\n' 0 >"$release_archive.sha256"
if SLURM_LOG_RELEASE_FIXTURE=$release_fixture \
   SLURM_LOG_RELEASE_ROOT=https://offline.invalid/releases \
   XDG_CONFIG_HOME=$home_dir/forged-config \
   XDG_STATE_HOME=$home_dir/forged-state \
   "$standalone/install.sh" --prefix "$home_dir/forged-prefix" \
   --no-setup --no-path-update >/dev/null 2>&1; then
    printf 'Installer accepted a forged release checksum\n' >&2
    exit 1
fi
test ! -e "$home_dir/forged-prefix/bin/slurm-log"
mv "$test_root/good.sha256" "$release_archive.sha256"

initial_binary=$test_root/old-slurm-log
cp "$binary" "$initial_binary"
printf 'offline-old-fixture' >>"$initial_binary"
chmod 755 "$initial_binary"
"$project_dir/install.sh" \
    --binary "$initial_binary" \
    --prefix "$home_dir/prefix" \
    --local-user alice \
    --remote-user alice.remote \
    --ssh-host cluster-alias \
    --no-setup \
    --no-path-update

installed=$home_dir/prefix/bin/slurm-log
config=$home_dir/config/slurm-log/config.json
test -x "$installed"
cmp "$installed" "$initial_binary"
grep -q '"localUser": "alice"' "$config"
grep -q '"remoteUser": "alice.remote"' "$config"
grep -q '"sshHost": "cluster-alias"' "$config"
test "$(stat -c %a "$config")" = 600

# Installer input must not become SSH options, JSON injection, or shell code.
marker=$test_root/injected
if "$project_dir/install.sh" \
    --binary "$binary" \
    --prefix "$home_dir/bad-prefix" \
    --ssh-host "-oProxyCommand=touch%20$marker" \
    --force-config --no-path-update >/dev/null 2>&1; then
    printf 'Unsafe SSH host was accepted\n' >&2
    exit 1
fi
test ! -e "$marker"

# The daemon can start without scheduler access and creates a private socket.
"$installed" daemon start >/dev/null
attempt=0
while ! "$installed" daemon status >/dev/null 2>&1; do
    attempt=$((attempt + 1))
    test "$attempt" -lt 100
    sleep 0.01
done
socket=$home_dir/state/slurm-log/daemon.sock
test -S "$socket"
test "$(stat -c %a "$socket")" = 600
"$installed" daemon stop >/dev/null

# The self-updater downloads and verifies the release entirely through local
# fixtures, atomically replaces an older valid image, preserves configuration,
# and carries a running daemon across to the new binary.
config_before=$(sha256sum "$config" | cut -d ' ' -f 1)
"$installed" daemon start >/dev/null
SLURM_LOG_RELEASE_FIXTURE=$release_fixture \
SLURM_LOG_RELEASE_ROOT=https://offline.invalid/releases \
    "$installed" update >/dev/null
cmp "$installed" "$binary"
test "$(sha256sum "$config" | cut -d ' ' -f 1)" = "$config_before"
"$installed" daemon status >/dev/null

# Identical updates are a no-op and do not cycle the daemon.
"$installed" update --binary "$binary" | grep -F 'already up to date' >/dev/null

# Downloader failures and fallbacks fail closed without changing the installed
# image. These fixtures exercise both supported clients and checksum/archive
# rejection without contacting a network.
curl_fail_bin=$test_root/curl-fail-bin
mkdir -p "$curl_fail_bin"
printf '#!/bin/sh\nexit 7\n' >"$curl_fail_bin/curl"
chmod 755 "$curl_fail_bin/curl"
if env PATH="$curl_fail_bin" "$installed" update >/dev/null 2>&1; then
    printf 'Failed curl update was accepted\n' >&2
    exit 1
fi

curl_error_bin=$test_root/curl-error-bin
mkdir -p "$curl_error_bin/curl"
if env PATH="$curl_error_bin" "$installed" update >/dev/null 2>&1; then
    printf 'Unexecutable curl update was accepted\n' >&2
    exit 1
fi

wget_fail_bin=$test_root/wget-fail-bin
mkdir -p "$wget_fail_bin"
printf '#!/bin/sh\nexit 8\n' >"$wget_fail_bin/wget"
chmod 755 "$wget_fail_bin/wget"
if env PATH="$wget_fail_bin" "$installed" update >/dev/null 2>&1; then
    printf 'Failed wget update was accepted\n' >&2
    exit 1
fi

wget_bin=$test_root/wget-bin
mkdir -p "$wget_bin"
cat >"$wget_bin/wget" <<'EOF'
#!/bin/sh
output=
url=
while [ "$#" -gt 0 ]; do
    case "$1" in
        -O) output=$2; shift 2 ;;
        *) url=$1; shift ;;
    esac
done
/bin/cp "$SLURM_LOG_RELEASE_FIXTURE/$(/usr/bin/basename "$url")" "$output"
EOF
chmod 755 "$wget_bin/wget"
ln -s /usr/bin/sha256sum "$wget_bin/sha256sum"
ln -s /usr/bin/tar "$wget_bin/tar"
ln -s /usr/bin/gzip "$wget_bin/gzip"
env PATH="$wget_bin" SLURM_LOG_RELEASE_FIXTURE="$release_fixture" \
    SLURM_LOG_RELEASE_ROOT=https://offline.invalid/releases \
    "$installed" update >/dev/null

bad_tar_fixture=$test_root/bad-tar-release
mkdir -p "$bad_tar_fixture"
printf 'not a tar archive\n' >"$bad_tar_fixture/$(basename "$release_archive")"
(cd "$bad_tar_fixture" && sha256sum "$(basename "$release_archive")" \
    >"$(basename "$release_archive").sha256")
if SLURM_LOG_RELEASE_FIXTURE="$bad_tar_fixture" \
   SLURM_LOG_RELEASE_ROOT=https://offline.invalid/releases \
   "$installed" update >/dev/null 2>&1; then
    printf 'Invalid update archive was accepted\n' >&2
    exit 1
fi

large_checksum_fixture=$test_root/large-checksum-release
mkdir -p "$large_checksum_fixture"
cp "$release_archive" "$large_checksum_fixture/$(basename "$release_archive")"
head -c 5000 /dev/zero >"$large_checksum_fixture/$(basename "$release_archive").sha256"
if SLURM_LOG_RELEASE_FIXTURE="$large_checksum_fixture" \
   SLURM_LOG_RELEASE_ROOT=https://offline.invalid/releases \
   "$installed" update >/dev/null 2>&1; then
    printf 'Oversized update checksum was accepted\n' >&2
    exit 1
fi

mismatch_fixture=$test_root/mismatch-release
mkdir -p "$mismatch_fixture"
cp "$release_archive" "$mismatch_fixture/$(basename "$release_archive")"
printf '%064d  %s\n' 0 "$(basename "$release_archive")" \
    >"$mismatch_fixture/$(basename "$release_archive").sha256"
if SLURM_LOG_RELEASE_FIXTURE="$mismatch_fixture" \
   SLURM_LOG_RELEASE_ROOT=https://offline.invalid/releases \
   "$installed" update >/dev/null 2>&1; then
    printf 'Mismatched update checksum was accepted\n' >&2
    exit 1
fi

bad_sha_bin=$test_root/bad-sha-bin
mkdir -p "$bad_sha_bin"
printf '#!/bin/sh\nexit 9\n' >"$bad_sha_bin/sha256sum"
chmod 755 "$bad_sha_bin/sha256sum"
if env PATH="$bad_sha_bin:$fake_bin:/usr/local/bin:/usr/bin:/bin" \
   SLURM_LOG_RELEASE_FIXTURE="$release_fixture" \
   SLURM_LOG_RELEASE_ROOT=https://offline.invalid/releases \
   "$installed" update >/dev/null 2>&1; then
    printf 'Update without a usable checksum tool was accepted\n' >&2
    exit 1
fi
cmp "$installed" "$binary"

# Explicit local binaries use the same atomic path without any download.
release_binary=$test_root/new-slurm-log
cp "$binary" "$release_binary"
printf 'offline-update-fixture' >>"$release_binary"
chmod 755 "$release_binary"
"$installed" update --binary "$release_binary" >/dev/null
cmp "$installed" "$release_binary"
test "$(sha256sum "$config" | cut -d ' ' -f 1)" = "$config_before"
"$installed" daemon status >/dev/null
"$installed" daemon stop >/dev/null

# Replacement/removal failures restore an active daemon and leave the binary
# intact. Directory permissions force the failures without privileged actions.
"$installed" daemon start >/dev/null
chmod 555 "$home_dir/prefix/bin"
if "$installed" update --binary "$binary" >/dev/null 2>&1; then
    chmod 755 "$home_dir/prefix/bin"
    printf 'Update unexpectedly replaced a binary in a read-only directory\n' >&2
    exit 1
fi
chmod 755 "$home_dir/prefix/bin"
cmp "$installed" "$release_binary"
"$installed" daemon status >/dev/null

chmod 555 "$home_dir/prefix/bin"
if "$installed" uninstall >/dev/null 2>&1; then
    chmod 755 "$home_dir/prefix/bin"
    printf 'Uninstall unexpectedly removed a binary from a read-only directory\n' >&2
    exit 1
fi
chmod 755 "$home_dir/prefix/bin"
test -x "$installed"
"$installed" daemon status >/dev/null
"$installed" daemon stop >/dev/null

# A corrupt candidate is rejected without replacing the working installation.
bad_release=$test_root/bad-slurm-log
printf '#!/bin/sh\nexit 9\n' >"$bad_release"
chmod 755 "$bad_release"
if "$installed" update --binary "$bad_release" >/dev/null 2>&1; then
    printf 'Corrupt update was accepted\n' >&2
    exit 1
fi
cmp "$installed" "$release_binary"

# A valid-looking older binary is rejected before the installed image changes.
old_release=$test_root/older-slurm-log
cat >"$old_release" <<'EOF'
#!/bin/sh
case "${1:-}" in
    --help) exit 0 ;;
    --version) printf 'slurm-log 0.1.1\n'; exit 0 ;;
esac
exit 0
EOF
chmod 755 "$old_release"
if "$installed" update --binary "$old_release" >/dev/null 2>&1; then
    printf 'Older update was accepted\n' >&2
    exit 1
fi
cmp "$installed" "$release_binary"

# Reinstall preserves existing configuration unless explicitly forced.
"$project_dir/install.sh" \
    --binary "$binary" \
    --prefix "$home_dir/prefix" \
    --remote-user replacement \
    --ssh-host replacement-host \
    --no-setup \
    --no-path-update >/dev/null
grep -q '"remoteUser": "alice.remote"' "$config"

"$installed" uninstall >/dev/null
test ! -e "$installed"
test -e "$config"

# Purging is a separate, explicit operation and removes only this user's app
# configuration/state after removing the second installed binary.
"$project_dir/install.sh" \
    --binary "$binary" \
    --prefix "$home_dir/purge-prefix" \
    --no-setup --no-path-update >/dev/null
purge_installed=$home_dir/purge-prefix/bin/slurm-log
test -x "$purge_installed"
"$purge_installed" uninstall --purge >/dev/null
test ! -e "$purge_installed"
test ! -e "$config"
test ! -e "$home_dir/state/slurm-log"

# A custom state file outside an app-named directory removes only known
# slurm-log siblings and leaves unrelated data untouched.
custom_prefix=$home_dir/custom-prefix
mkdir -p "$custom_prefix/bin" "$test_root/custom-state"
install -m 755 "$binary" "$custom_prefix/bin/slurm-log"
custom_config=$test_root/custom-config.json
custom_state=$test_root/custom-state/custom.json
cat >"$custom_config" <<EOF
{"clusters":[{"name":"local","transport":"local","user":"alice","workingDirectory":"$test_root","accounting":false}],"statePath":"$custom_state"}
EOF
printf '{}\n' >"$custom_state"
: >"${custom_state%.*}.lock"
: >"${custom_state%.*}.archive-cache.json"
: >"$test_root/custom-state/daemon.lock"
printf 'keep\n' >"$test_root/custom-state/unrelated"
SLURM_LOG_CONFIG="$custom_config" SLURM_LOG_STATE="$custom_state" \
    "$custom_prefix/bin/slurm-log" uninstall --purge >/dev/null
test ! -e "$custom_prefix/bin/slurm-log"
test ! -e "$custom_config"
test ! -e "$custom_state"
test ! -e "${custom_state%.*}.lock"
test ! -e "${custom_state%.*}.archive-cache.json"
test ! -e "$test_root/custom-state/daemon.lock"
test -e "$test_root/custom-state/unrelated"

# Portable source must not contain maintainer identity or runtime artifacts.
if grep -R -n -E 'c01bima|/home/mansur|/storage1/mansur' \
    "$project_dir/src" "$project_dir/README.md" "$project_dir/install.sh" "$project_dir/update.sh"; then
    printf 'Personal data found in portable package\n' >&2
    exit 1
fi
printf 'package_smoke: ok\n'
