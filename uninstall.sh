#!/bin/sh
# Removes the current user's slurm-log binary. Configuration and job history
# are preserved unless --purge is explicitly supplied.
#
# Usage: ./uninstall.sh [--prefix DIR] [--purge]

set -eu
prefix=${HOME}/.local
purge=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --prefix) prefix=$2; shift 2 ;;
        --purge) purge=1; shift ;;
        -h|--help) sed -n '2,5p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) printf 'Unknown option: %s\n' "$1" >&2; exit 2 ;;
    esac
done

if [ -x "$prefix/bin/slurm-log" ]; then
    "$prefix/bin/slurm-log" daemon stop >/dev/null 2>&1 || true
    rm -f "$prefix/bin/slurm-log"
fi
printf 'Removed %s/bin/slurm-log\n' "$prefix"

if [ "$purge" -eq 1 ]; then
    config_home=${XDG_CONFIG_HOME:-${HOME}/.config}
    state_home=${XDG_STATE_HOME:-${HOME}/.local/state}
    rm -rf "$config_home/slurm-log" "$state_home/slurm-log"
    printf 'Removed configuration and state.\n'
else
    printf 'Configuration and state were preserved. Use --purge to remove them.\n'
fi

