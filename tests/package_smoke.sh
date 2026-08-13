#!/bin/sh
# Offline end-to-end test for installation, configuration security, daemon
# lifecycle, update behavior, uninstall preservation, and package hygiene.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
test_root=$(mktemp -d)
case "$test_root" in /tmp/*) ;; *) printf 'Unsafe temp path\n' >&2; exit 1 ;; esac
trap 'rm -rf "$test_root"' EXIT HUP INT TERM

# A fresh ephemeral PKCS#8 key exists only beneath this test's private temp
# directory. Cargo receives the public half only through the explicit fixture
# build flag; production builds take their immutable verifier key from the
# reviewed source PEM and never accept a runtime key override.
real_cargo=$(command -v cargo)
test_private_key=$test_root/release-private.pem
test_public_pem=$test_root/release-public.pem
umask 077
openssl genpkey -algorithm ED25519 -out "$test_private_key" >/dev/null 2>&1
openssl pkey -in "$test_private_key" -pubout -out "$test_public_pem" >/dev/null 2>&1
test_public_key=$(openssl pkey -pubin -in "$test_public_pem" -pubout -outform DER | \
    tail -c 32 | od -An -tx1 | tr -d ' \n')
[ "${#test_public_key}" -eq 64 ]
CARGO_NET_OFFLINE=true SLURM_LOG_TEST_BUILD=1 \
SLURM_LOG_TEST_RELEASE_PUBLIC_KEY="$test_public_key" \
    "$real_cargo" build --locked --offline --release --manifest-path "$project_dir/Cargo.toml" --bin slurm-log >/dev/null
binary=${SLURM_LOG_TEST_BINARY:-$project_dir/target/release/slurm-log}
test -s "$test_public_pem"

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

# A normal release must accept the reviewed production anchor. Then repeat the
# same packaging operation from a source copy whose anchor was replaced by the
# sentinel, proving that production packaging fails for an unconfigured key
# even when Cargo and a seemingly valid binary are available.
production_probe=$test_root/production-probe.tar.gz
SLURM_LOG_ALLOW_PACKAGE_BINARY=1 \
SLURM_LOG_PACKAGE_BINARY=$binary \
    "$project_dir/package.sh" "$production_probe" >/dev/null
test -s "$production_probe"

unconfigured_project=$test_root/unconfigured-project
mkdir -p "$unconfigured_project"
for item in Cargo.toml Cargo.lock build.rs release-public-key.pem deny.toml README.md CHANGELOG.md LICENSE install.sh update.sh uninstall.sh package.sh test-all.sh security-audit.sh config.example.json src tests; do
    cp -R "$project_dir/$item" "$unconfigured_project/"
done
printf '%s\n' UNCONFIGURED >"$unconfigured_project/release-public-key.pem"
if SLURM_LOG_ALLOW_PACKAGE_BINARY=1 \
   SLURM_LOG_PACKAGE_BINARY=$binary \
   "$unconfigured_project/package.sh" "$test_root/untrusted.tar.gz" >/dev/null 2>&1; then
    printf 'Production packaging accepted an unconfigured release key\n' >&2
    exit 1
fi

# A release package carries a matching checksum and can be consumed by a
# standalone copy of install.sh without Cargo or network access.
release_fixture=$test_root/release
release_archive=$release_fixture/slurm-log-linux-x86_64.tar.gz
mkdir -p "$release_fixture"
SLURM_LOG_TEST_BUILD=1 \
SLURM_LOG_TEST_RELEASE_PUBLIC_KEY="$test_public_key" \
SLURM_LOG_TARGET=x86_64-unknown-linux-musl \
SLURM_LOG_ALLOW_PACKAGE_BINARY=1 \
SLURM_LOG_PACKAGE_BINARY=$binary "$project_dir/package.sh" "$release_archive" >/dev/null
openssl pkeyutl -sign -inkey "$test_private_key" -rawin \
    -in "$release_archive.manifest" -out "$release_archive.manifest.sig"
test -s "$release_archive"
test -s "$release_archive.sha256"
test -s "$release_archive.manifest"
test "$(wc -c <"$release_archive.manifest.sig" | tr -d ' ')" = 64
expected=$(awk 'NR == 1 { print $1 }' "$release_archive.sha256")
test "$expected" = "$(sha256sum "$release_archive" | awk '{ print $1 }')"
tar -tzf "$release_archive" | grep -Fx 'slurm-log/bin/slurm-log' >/dev/null

copy_signed_fixture() {
    destination=$1
    mkdir -p "$destination"
    for suffix in '' .sha256 .manifest .manifest.sig; do
        cp "$release_archive$suffix" "$destination/$(basename "$release_archive")$suffix"
    done
}

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
if [ -n "${SLURM_LOG_CURL_LOG:-}" ]; then
    printf '%s\n' "$(basename -- "$url")" >>"$SLURM_LOG_CURL_LOG"
fi
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
"$standalone/install.sh" --help | grep -F -- '--no-path-update' >/dev/null
if "$standalone/install.sh" --version '../unsafe' >/dev/null 2>&1; then
    printf 'Installer accepted an unsafe release tag\n' >&2
    exit 1
fi
SLURM_LOG_RELEASE_FIXTURE=$release_fixture \
SLURM_LOG_RELEASE_ROOT=https://offline.invalid/releases \
XDG_CONFIG_HOME=$home_dir/download-config \
XDG_STATE_HOME=$home_dir/download-state \
    "$standalone/install.sh" --release-public-key "$test_public_pem" --prefix "$home_dir/download-prefix" \
    --no-setup --no-path-update >/dev/null
cmp "$home_dir/download-prefix/bin/slurm-log" "$binary"

# A forged checksum must fail closed and leave no installed binary.
cp "$release_archive.sha256" "$test_root/good.sha256"
printf '%064d  slurm-log-linux-x86_64.tar.gz\n' 0 >"$release_archive.sha256"
if SLURM_LOG_RELEASE_FIXTURE=$release_fixture \
   SLURM_LOG_RELEASE_ROOT=https://offline.invalid/releases \
   XDG_CONFIG_HOME=$home_dir/forged-config \
   XDG_STATE_HOME=$home_dir/forged-state \
   "$standalone/install.sh" --release-public-key "$test_public_pem" --prefix "$home_dir/forged-prefix" \
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

# Downloader and signed-artifact failures fail closed without changing the
# image. These fixtures use only local fake curl and release files.
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

# Both a forged manifest and a forged detached signature fail before archive
# extraction or candidate execution.
forged_manifest_fixture=$test_root/forged-manifest-release
copy_signed_fixture "$forged_manifest_fixture"
printf 'forged\n' >>"$forged_manifest_fixture/$(basename "$release_archive").manifest"
forged_manifest_curl_log=$test_root/forged-manifest-curl.log
if SLURM_LOG_RELEASE_FIXTURE="$forged_manifest_fixture" \
   SLURM_LOG_CURL_LOG="$forged_manifest_curl_log" \
   SLURM_LOG_RELEASE_ROOT=https://offline.invalid/releases \
   "$installed" update >/dev/null 2>&1; then
    printf 'Forged signed manifest was accepted\n' >&2
    exit 1
fi
! grep -Fx "$(basename "$release_archive")" "$forged_manifest_curl_log" >/dev/null

forged_signature_fixture=$test_root/forged-signature-release
copy_signed_fixture "$forged_signature_fixture"
printf x >"$forged_signature_fixture/$(basename "$release_archive").manifest.sig"
if SLURM_LOG_RELEASE_FIXTURE="$forged_signature_fixture" \
   SLURM_LOG_RELEASE_ROOT=https://offline.invalid/releases \
   "$installed" update >/dev/null 2>&1; then
    printf 'Forged manifest signature was accepted\n' >&2
    exit 1
fi

# A manifest and checksum that are valid for a non-tar payload still fail at
# bounded extraction; this proves signature verification alone is not treated
# as permission to execute/install arbitrary bytes.
bad_tar_fixture=$test_root/bad-tar-release
mkdir -p "$bad_tar_fixture"
bad_asset=$(basename "$release_archive")
printf 'not a tar archive\n' >"$bad_tar_fixture/$bad_asset"
bad_digest=$(sha256sum "$bad_tar_fixture/$bad_asset" | awk '{ print $1 }')
bad_size=$(wc -c <"$bad_tar_fixture/$bad_asset" | tr -d ' ')
printf 'slurm-log-release-v1\nversion=0.2.3\ntarget=x86_64-unknown-linux-musl\narchive=%s\nsha256=%s\nsize=%s\n' \
    "$bad_asset" "$bad_digest" "$bad_size" >"$bad_tar_fixture/$bad_asset.manifest"
printf '%s  %s\n' "$bad_digest" "$bad_asset" >"$bad_tar_fixture/$bad_asset.sha256"
openssl pkeyutl -sign -inkey "$test_private_key" -rawin \
    -in "$bad_tar_fixture/$bad_asset.manifest" \
    -out "$bad_tar_fixture/$bad_asset.manifest.sig"
if SLURM_LOG_RELEASE_FIXTURE="$bad_tar_fixture" \
   SLURM_LOG_RELEASE_ROOT=https://offline.invalid/releases \
   "$installed" update >/dev/null 2>&1; then
    printf 'Invalid update archive was accepted\n' >&2
    exit 1
fi

large_checksum_fixture=$test_root/large-checksum-release
copy_signed_fixture "$large_checksum_fixture"
head -c 5000 /dev/zero >"$large_checksum_fixture/$(basename "$release_archive").sha256"
if SLURM_LOG_RELEASE_FIXTURE="$large_checksum_fixture" \
   SLURM_LOG_RELEASE_ROOT=https://offline.invalid/releases \
   "$installed" update >/dev/null 2>&1; then
    printf 'Oversized update checksum was accepted\n' >&2
    exit 1
fi

mismatch_fixture=$test_root/mismatch-release
copy_signed_fixture "$mismatch_fixture"
printf '%064d  %s\n' 0 "$(basename "$release_archive")" \
    >"$mismatch_fixture/$(basename "$release_archive").sha256"
if SLURM_LOG_RELEASE_FIXTURE="$mismatch_fixture" \
   SLURM_LOG_RELEASE_ROOT=https://offline.invalid/releases \
   "$installed" update >/dev/null 2>&1; then
    printf 'Mismatched update checksum was accepted\n' >&2
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

# Candidate validation inspects the original path before canonicalization, so a
# symlink cannot turn an explicit or archive candidate into another executable.
linked_release=$test_root/linked-release
ln -s "$binary" "$linked_release"
if "$installed" update --binary "$linked_release" >/dev/null 2>&1; then
    printf 'Symlinked update candidate was accepted\n' >&2
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

# The recovery updater has the same monotonic-version default as `slurm-log
# update`; its override is explicit and is not exercised by this test.
if "$project_dir/update.sh" --prefix "$home_dir/prefix" --binary "$old_release" >/dev/null 2>&1; then
    printf 'Standalone updater accepted a downgrade\n' >&2
    exit 1
fi

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
