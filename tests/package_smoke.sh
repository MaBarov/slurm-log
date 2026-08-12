#!/bin/sh
# Offline end-to-end test for installation, configuration security, daemon
# lifecycle, update behavior, uninstall preservation, and package hygiene.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
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
"$project_dir/package.sh" "$release_archive" >/dev/null
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
cmp "$home_dir/download-prefix/bin/slurm-log" "$project_dir/target/release/slurm-log"

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

"$project_dir/install.sh" \
    --binary "$project_dir/target/release/slurm-log" \
    --prefix "$home_dir/prefix" \
    --local-user alice \
    --remote-user alice.remote \
    --ssh-host cluster-alias \
    --no-setup \
    --no-path-update

installed=$home_dir/prefix/bin/slurm-log
config=$home_dir/config/slurm-log/config.json
test -x "$installed"
cmp "$installed" "$project_dir/target/release/slurm-log"
grep -q '"localUser": "alice"' "$config"
grep -q '"remoteUser": "alice.remote"' "$config"
grep -q '"sshHost": "cluster-alias"' "$config"
test "$(stat -c %a "$config")" = 600

# Installer input must not become SSH options, JSON injection, or shell code.
marker=$test_root/injected
if "$project_dir/install.sh" \
    --binary "$project_dir/target/release/slurm-log" \
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

# A release update is atomic, preserves private configuration, and carries a
# running daemon across to the new binary. Appending bytes keeps the test fully
# offline while producing a distinct, still-valid ELF release image.
release_binary=$test_root/new-slurm-log
cp "$project_dir/target/release/slurm-log" "$release_binary"
printf 'offline-update-fixture' >>"$release_binary"
chmod 755 "$release_binary"
config_before=$(sha256sum "$config" | cut -d ' ' -f 1)
"$installed" daemon start >/dev/null
"$project_dir/update.sh" --prefix "$home_dir/prefix" --binary "$release_binary" >/dev/null
cmp "$installed" "$release_binary"
test "$(sha256sum "$config" | cut -d ' ' -f 1)" = "$config_before"
"$installed" daemon status >/dev/null
"$installed" daemon stop >/dev/null

# A corrupt candidate is rejected without replacing the working installation.
bad_release=$test_root/bad-slurm-log
printf '#!/bin/sh\nexit 9\n' >"$bad_release"
chmod 755 "$bad_release"
if "$project_dir/update.sh" --prefix "$home_dir/prefix" --binary "$bad_release" >/dev/null 2>&1; then
    printf 'Corrupt update was accepted\n' >&2
    exit 1
fi
cmp "$installed" "$release_binary"

# Reinstall preserves existing configuration unless explicitly forced.
"$project_dir/install.sh" \
    --binary "$project_dir/target/release/slurm-log" \
    --prefix "$home_dir/prefix" \
    --remote-user replacement \
    --ssh-host replacement-host \
    --no-setup \
    --no-path-update >/dev/null
grep -q '"remoteUser": "alice.remote"' "$config"

"$project_dir/uninstall.sh" --prefix "$home_dir/prefix" >/dev/null
test ! -e "$installed"
test -e "$config"

# Portable source must not contain maintainer identity or runtime artifacts.
if grep -R -n -E 'c01bima|/home/mansur|/storage1/mansur' \
    "$project_dir/src" "$project_dir/README.md" "$project_dir/install.sh" "$project_dir/update.sh"; then
    printf 'Personal data found in portable package\n' >&2
    exit 1
fi
printf 'package_smoke: ok\n'
