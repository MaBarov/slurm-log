#!/bin/sh
# Guided MCP-client registration coverage without a scheduler dependency.
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
binary=${SLURM_LOG_TEST_BINARY:-$project_dir/target/release/slurm-log}
test_root=$(mktemp -d)
case "$test_root" in /tmp/*) ;; *) exit 1 ;; esac
trap 'rm -rf "$test_root"' EXIT HUP INT TERM
fake_bin=$test_root/bin
mkdir -p "$fake_bin" "$test_root/clients" "$test_root/work" "$test_root/state"

cat >"$test_root/config.json" <<EOF
{"clusters":[{"name":"alpha","transport":"local","user":"offline","workingDirectory":"$test_root/work","accounting":false}],"sbatchBanks":[],"statePath":"$test_root/state/state.json"}
EOF
export PATH="$fake_bin:/usr/local/bin:/usr/bin:/bin"
export HOME=$test_root/home
export SLURM_LOG_CONFIG=$test_root/config.json
export MCP_CLIENT_CALLS=$test_root/client-calls
export MCP_CLIENT_STATE=$test_root/clients

cat >"$fake_bin/codex" <<'EOF'
#!/bin/sh
printf 'codex %s\n' "$*" >>"$MCP_CLIENT_CALLS"
case "$*" in
  'mcp get slurm-log --json') test -f "$MCP_CLIENT_STATE/codex" ;;
  'mcp add slurm-log -- '*)
    case "${MCP_CLIENT_MODE:-normal}" in
      add-fail) printf 'offline add failure\n' >&2; exit 9 ;;
      verify-fail) : ;;
      *) touch "$MCP_CLIENT_STATE/codex" ;;
    esac
    ;;
  'mcp remove slurm-log')
    test "${MCP_CLIENT_MODE:-normal}" = remove-sticky || rm -f "$MCP_CLIENT_STATE/codex"
    ;;
  *) exit 2 ;;
esac
EOF
cat >"$fake_bin/claude" <<'EOF'
#!/bin/sh
printf 'claude %s\n' "$*" >>"$MCP_CLIENT_CALLS"
case "$*" in
  'mcp get slurm-log') test -f "$MCP_CLIENT_STATE/claude" ;;
  'mcp add --scope user slurm-log -- '*) touch "$MCP_CLIENT_STATE/claude" ;;
  'mcp remove --scope user slurm-log') rm -f "$MCP_CLIENT_STATE/claude" ;;
  *) exit 2 ;;
esac
EOF
chmod 755 "$fake_bin/codex" "$fake_bin/claude"
: >"$MCP_CLIENT_CALLS"

"$binary" mcp setup </dev/null >"$test_root/setup.out"
grep -F 'codex mcp add slurm-log -- ' "$test_root/setup.out" >/dev/null
grep -F 'claude mcp add --scope user slurm-log -- ' "$test_root/setup.out" >/dev/null
grep -F '"mcpServers"' "$test_root/setup.out" >/dev/null
! grep -F ' mcp add ' "$MCP_CLIENT_CALLS" >/dev/null
touch "$MCP_CLIENT_STATE/codex" "$MCP_CLIENT_STATE/claude"
"$binary" mcp setup </dev/null | grep -F 'left unchanged' >/dev/null
"$binary" mcp unregister >/dev/null
test ! -e "$MCP_CLIENT_STATE/codex" && test ! -e "$MCP_CLIENT_STATE/claude"
"$binary" mcp unregister | grep -F 'No supported client has a `slurm-log` registration.' >/dev/null
PATH=/usr/bin:/bin "$binary" mcp setup </dev/null |
    grep -F 'No supported command-line MCP client was found on PATH.' >/dev/null

# Interactive registration and replacement require an affirmative response.
python3 "$project_dir/tests/pty_sequence.py" "$test_root/setup-interactive.out" \
    'Run this user-scoped command? [y/N] ' 'y\r' \
    'Run this user-scoped command? [y/N] ' 'yes\r' -- "$binary" mcp setup
test -e "$MCP_CLIENT_STATE/codex" && test -e "$MCP_CLIENT_STATE/claude"
grep -F 'Verified `slurm-log` registration.' "$test_root/setup-interactive.out" >/dev/null
python3 "$project_dir/tests/pty_sequence.py" "$test_root/setup-replace.out" \
    'Replace it? [y/N] ' 'y\r' 'Replace it? [y/N] ' 'y\r' -- "$binary" mcp setup
test "$(grep -c 'Removed `slurm-log` registration.' "$test_root/setup-replace.out")" -eq 2

# Command, verification, and removal failures are surfaced to the user.
rm -f "$MCP_CLIENT_STATE/codex" "$MCP_CLIENT_STATE/claude"
if MCP_CLIENT_MODE=add-fail python3 "$project_dir/tests/pty_sequence.py" \
    "$test_root/setup-add-fail.out" 'Run this user-scoped command? [y/N] ' 'y\r' -- "$binary" mcp setup; then exit 1; fi
grep -F 'Codex command failed: offline add failure' "$test_root/setup-add-fail.out" >/dev/null
if MCP_CLIENT_MODE=verify-fail python3 "$project_dir/tests/pty_sequence.py" \
    "$test_root/setup-verify-fail.out" 'Run this user-scoped command? [y/N] ' 'y\r' -- "$binary" mcp setup; then exit 1; fi
grep -F 'Codex registration could not be verified' "$test_root/setup-verify-fail.out" >/dev/null
touch "$MCP_CLIENT_STATE/codex"
if MCP_CLIENT_MODE=remove-sticky python3 "$project_dir/tests/pty_sequence.py" \
    "$test_root/setup-remove-fail.out" 'Replace it? [y/N] ' 'y\r' -- "$binary" mcp setup; then exit 1; fi
grep -F 'Codex registration still exists after removal' "$test_root/setup-remove-fail.out" >/dev/null

printf 'mcp_setup: ok (guided Codex/Claude registration; fully offline)\n'
