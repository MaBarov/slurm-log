#!/bin/sh
# Creates a portable source + native binary release archive.
# Usage: ./package.sh [OUTPUT.tar.gz]

set -eu
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
architecture=$(uname -m)
output=${1:-$script_dir/dist/slurm-log-linux-$architecture.tar.gz}
mkdir -p "$(dirname -- "$output")"
# Production releases use the immutable public key reviewed in source. Tests
# may opt into a separately named compile-time key so no production key/secret
# is ever placed in a fixture or obtained from a release mirror.
if [ "${SLURM_LOG_TEST_BUILD:-}" = 1 ]; then
    release_public_key=${SLURM_LOG_TEST_RELEASE_PUBLIC_KEY:-}
    case "$release_public_key" in
        *[!0-9A-Fa-f]*|'')
            printf '%s\n' 'Test release public key must be exactly 64 hexadecimal characters.' >&2
            exit 2
            ;;
    esac
    [ "${#release_public_key}" -eq 64 ] || {
        printf '%s\n' 'Test release public key must be exactly 64 hexadecimal characters.' >&2
        exit 2
    }
else
    release_public_key=$script_dir/release-public-key.pem
    [ -f "$release_public_key" ] && [ ! -L "$release_public_key" ] || {
        printf '%s\n' 'Release public key PEM is missing or unsafe.' >&2
        exit 2
    }
    if grep -qx 'UNCONFIGURED' "$release_public_key" || ! command -v openssl >/dev/null 2>&1 || \
       ! openssl pkey -pubin -in "$release_public_key" -pubout >/dev/null 2>&1; then
        printf '%s\n' 'Release public key is unconfigured; make a reviewed source commit before packaging a release.' >&2
        exit 2
    fi
fi
# Always invoke Cargo: its incremental fingerprint check is fast when nothing
# changed and guarantees the packaged binary matches the included source/lock.
build_target=${SLURM_LOG_TARGET:-}
if [ -n "$build_target" ]; then
    cargo build --locked --release --target "$build_target" --manifest-path "$script_dir/Cargo.toml"
    release_binary=$script_dir/target/$build_target/release/slurm-log
else
    cargo build --locked --release --manifest-path "$script_dir/Cargo.toml"
    release_binary=$script_dir/target/release/slurm-log
fi
# Coverage and package tests may supply their freshly instrumented binary. The
# Cargo check above still runs, so ordinary packages cannot silently reuse a
# stale artifact.
if [ -n "${SLURM_LOG_PACKAGE_BINARY:-}" ]; then
    [ "${SLURM_LOG_ALLOW_PACKAGE_BINARY:-}" = 1 ] || {
        printf '%s\n' 'SLURM_LOG_PACKAGE_BINARY is test-only; set SLURM_LOG_ALLOW_PACKAGE_BINARY=1 explicitly.' >&2
        exit 2
    }
    release_binary=$SLURM_LOG_PACKAGE_BINARY
fi
[ -f "$release_binary" ] && [ -x "$release_binary" ] && [ ! -L "$release_binary" ] || {
    printf 'Release binary is missing or unsafe: %s\n' "$release_binary" >&2
    exit 1
}
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM
mkdir -p "$tmp_dir/slurm-log/bin"
for item in Cargo.toml Cargo.lock build.rs release-public-key.pem deny.toml README.md CHANGELOG.md LICENSE install.sh update.sh uninstall.sh package.sh test-all.sh security-audit.sh config.example.json src tests; do
    cp -R "$script_dir/$item" "$tmp_dir/slurm-log/"
done
install -m 755 "$release_binary" "$tmp_dir/slurm-log/bin/slurm-log"
tar -C "$tmp_dir" -czf "$output" slurm-log
output_dir=$(dirname -- "$output")
output_name=$(basename -- "$output")
(cd "$output_dir" && sha256sum "$output_name" >"$output_name.sha256")
release_version=$(sed -n 's/^version = "\([0-9][0-9.]*\)"$/\1/p' "$script_dir/Cargo.toml")
[ -n "$release_version" ] || { printf '%s\n' 'Could not determine release version.' >&2; exit 2; }
release_target=${build_target:-native-$architecture}
archive_size=$(wc -c <"$output" | tr -d ' ')
archive_digest=$(awk 'NR == 1 { print $1 }' "$output.sha256")
manifest=$output.manifest
rm -f "$manifest.sig"
printf 'slurm-log-release-v1\nversion=%s\ntarget=%s\narchive=%s\nsha256=%s\nsize=%s\n' \
    "$release_version" "$release_target" "$output_name" "$archive_digest" "$archive_size" >"$manifest"
printf 'Created %s\n' "$output"
printf 'Checksum %s.sha256\n' "$output"
printf 'Unsigned manifest %s (the protected OpenSSL signing job must create %s.sig)\n' "$manifest" "$manifest"
