#!/bin/sh
# Offline structural gate for release privilege separation and trust anchors.

set -eu
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
workflow=$project_dir/.github/workflows/release.yml

grep -F 'cargo install --locked --version 0.22.2 cargo-audit' "$workflow" >/dev/null
grep -F 'cargo install --locked --version 0.20.2 cargo-deny' "$workflow" >/dev/null
grep -F 'cargo install --locked --version 0.13.0 cargo-geiger' "$workflow" >/dev/null
grep -F 'cargo install cargo-llvm-cov --version 0.8.7 --locked' "$workflow" >/dev/null
grep -F 'run: ./coverage.sh' "$workflow" >/dev/null
grep -F 'actions/upload-artifact@65462800fd760344b1a7b4382951275a0abb4808' "$workflow" >/dev/null
grep -F 'actions/download-artifact@fa0a91b85d4f404e444e00e005971372dc801d16' "$workflow" >/dev/null

sign_block=$(sed -n '/^  sign:/,/^  publish:/p' "$workflow")
printf '%s\n' "$sign_block" | grep -F 'SLURM_LOG_RELEASE_SIGNING_KEY_PEM' >/dev/null
printf '%s\n' "$sign_block" | grep -F 'openssl pkeyutl -sign' >/dev/null
printf '%s\n' "$sign_block" | grep -F 'openssl pkeyutl -verify' >/dev/null
printf '%s\n' "$sign_block" | grep -F 'cmp -s "$trusted_der" "$derived_der"' >/dev/null
printf '%s\n' "$sign_block" | grep -F 'cmp -s expected.manifest "$manifest"' >/dev/null
printf '%s\n' "$sign_block" | grep -F 'rm -f "$private_key"' >/dev/null
if printf '%s\n' "$sign_block" | grep -Eq 'cargo (install|build|test|run)|release-sign( |$|/)|SLURM_LOG_RELEASE_SIGNING_SEED'; then
    printf 'Signing job still exposes signing material to repository Cargo code\n' >&2
    exit 1
fi

# Only the final publisher has contents:write; it only transfers/checks signed
# artifacts and invokes gh, never compiles, audits, installs Cargo tools, or
# checks out mutable source. It verifies the signature against an independent
# protected public-key PEM before its write-token step is reached.
awk '
    /^  publish:/ { publisher = 1 }
    publisher && /uses: actions\/checkout/ { failed = 1 }
    publisher && /(cargo (install|build|test|run)|security-audit\.sh|package\.sh)/ { failed = 1 }
    END { exit failed }
' "$workflow"
publish_block=$(sed -n '/^  publish:/,$p' "$workflow")
printf '%s\n' "$publish_block" | grep -F 'contents: write' >/dev/null
printf '%s\n' "$publish_block" | grep -F 'name: release-signed' >/dev/null
printf '%s\n' "$publish_block" | grep -F 'SLURM_LOG_RELEASE_PUBLIC_KEY_PEM' >/dev/null
printf '%s\n' "$publish_block" | grep -F 'openssl pkeyutl -verify' >/dev/null
printf '%s\n' "$publish_block" | grep -F '.manifest.sig' >/dev/null
printf '%s\n' "$publish_block" | grep -F -- '--repo "$GITHUB_REPOSITORY"' >/dev/null

# The one-line curl installer must travel with every release so that
# `.../releases/latest/download/install.sh | bash` stays pinned to the signed
# set published by the same workflow.
printf '%s\n' "$sign_block" | grep -F 'install.sh' >/dev/null
printf '%s\n' "$publish_block" | grep -F 'dist/install.sh' >/dev/null

# A production public key is an immutable reviewed source input. The explicit
# test-only compile flag is the sole fixture escape hatch.
key_file=$project_dir/release-public-key.pem
key=$(tr -d '\r\n' <"$key_file")
case "$key" in
    UNCONFIGURED) ;;
    *)
        openssl pkey -pubin -in "$key_file" -pubout -outform DER >"${TMPDIR:-/tmp}/slurm-log-release-key.$$"
        test "$(wc -c <"${TMPDIR:-/tmp}/slurm-log-release-key.$$" | tr -d ' ')" = 44
        rm -f "${TMPDIR:-/tmp}/slurm-log-release-key.$$"
        ;;
esac
! grep -F 'env::var("SLURM_LOG_RELEASE_PUBLIC_KEY")' "$project_dir/src/release_auth.rs" >/dev/null
grep -F 'include_str!("../release-public-key.pem")' "$project_dir/src/release_auth.rs" >/dev/null
grep -F 'SLURM_LOG_TEST_BUILD' "$project_dir/build.rs" >/dev/null
grep -F 'SLURM_LOG_TEST_RELEASE_PUBLIC_KEY' "$project_dir/build.rs" >/dev/null
test ! -e "$project_dir/src/bin/release-sign.rs"

# The piped installer embeds the same reviewed trust anchor as the updater;
# drift between install.sh and release-public-key.pem must fail this gate.
key_line=$(sed -n '2p' "$key_file" | tr -d '\r')
grep -F "'$key_line'" "$project_dir/install.sh" >/dev/null
grep -F "'-----BEGIN PUBLIC KEY-----'" "$project_dir/install.sh" >/dev/null
grep -F "'-----END PUBLIC KEY-----'" "$project_dir/install.sh" >/dev/null

printf '%s\n' 'release_workflow: ok (pinned verification, no-Cargo signing, immutable trust anchor)'
