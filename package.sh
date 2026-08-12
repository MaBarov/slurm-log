#!/bin/sh
# Creates a source + native binary archive suitable for sending to colleagues.
# Usage: ./package.sh [OUTPUT.tar.gz]

set -eu
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
architecture=$(uname -m)
output=${1:-$script_dir/dist/slurm-log-linux-$architecture.tar.gz}
mkdir -p "$(dirname -- "$output")"
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
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM
mkdir -p "$tmp_dir/slurm-log/bin"
for item in Cargo.toml Cargo.lock deny.toml README.md LICENSE install.sh update.sh uninstall.sh package.sh test-all.sh security-audit.sh config.example.json src tests; do
    cp -R "$script_dir/$item" "$tmp_dir/slurm-log/"
done
install -m 755 "$release_binary" "$tmp_dir/slurm-log/bin/slurm-log"
tar -C "$tmp_dir" -czf "$output" slurm-log
output_dir=$(dirname -- "$output")
output_name=$(basename -- "$output")
(cd "$output_dir" && sha256sum "$output_name" >"$output_name.sha256")
printf 'Created %s\n' "$output"
printf 'Checksum %s.sha256\n' "$output"
