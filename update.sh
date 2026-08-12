#!/bin/sh
# Updates an existing slurm-log installation from this release.
#
# Put this script beside either `slurm-log` or `bin/slurm-log`, then run:
#   ./update.sh
#
# Options:
#   --prefix DIR    Installed prefix (default: ~/.local)
#   --binary FILE   Explicit new slurm-log binary
#   -h, --help      Show this help
#
# Configuration, caches, and job history are never replaced. The installed
# daemon is stopped during the atomic swap and restarted only if it was running.

set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
prefix=${HOME}/.local
binary=

usage() {
    sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --prefix)
            value=${2-}
            [ -n "$value" ] || { printf '%s\n' '--prefix requires a directory' >&2; exit 2; }
            prefix=$value
            shift 2
            ;;
        --binary)
            value=${2-}
            [ -n "$value" ] || { printf '%s\n' '--binary requires a file' >&2; exit 2; }
            binary=$value
            shift 2
            ;;
        -h|--help) usage; exit 0 ;;
        *) printf 'Unknown option: %s\n' "$1" >&2; usage >&2; exit 2 ;;
    esac
done

if [ -z "$binary" ]; then
    if [ -x "$script_dir/bin/slurm-log" ]; then
        binary=$script_dir/bin/slurm-log
    elif [ -x "$script_dir/slurm-log" ]; then
        binary=$script_dir/slurm-log
    else
        printf 'No release binary found beside update.sh. Use --binary FILE.\n' >&2
        exit 1
    fi
fi

target=$prefix/bin/slurm-log
[ -x "$binary" ] || { printf 'New binary is not executable: %s\n' "$binary" >&2; exit 1; }
[ -x "$target" ] || { printf 'slurm-log is not installed at %s\nRun install.sh first.\n' "$target" >&2; exit 1; }

# Reject a corrupt or incompatible release before touching the running install.
if ! "$binary" --help >/dev/null 2>&1; then
    printf 'The new binary failed its startup check; installation was not changed.\n' >&2
    exit 1
fi

if cmp -s "$binary" "$target"; then
    printf 'slurm-log is already up to date: %s\n' "$target"
    exit 0
fi

was_running=0
if "$target" daemon status >/dev/null 2>&1; then
    was_running=1
    "$target" daemon stop >/dev/null 2>&1 || true
fi

umask 077
temporary=$prefix/bin/.slurm-log.update.$$
cleanup() {
    rm -f "$temporary"
}
trap cleanup EXIT HUP INT TERM
install -m 755 "$binary" "$temporary"
mv -f "$temporary" "$target"

if [ "$was_running" -eq 1 ]; then
    "$target" daemon start >/dev/null
fi

printf 'Updated: %s\n' "$target"
printf 'Configuration and job history were preserved.\n'
