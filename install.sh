#!/bin/sh
# slurm-log portable installer
#
# Builds and installs slurm-log for the current user, writes an owner-scoped
# configuration, and checks runtime commands. It never copies another user's
# state, credentials, daemon socket, or job history.
#
# Quick setup:
#   ./install.sh
#
# Useful options:
#   --local-user USER       Local SLURM owner (default: $USER)
#   --remote-user USER      Remote SLURM owner (default: local user)
#   --ssh-host HOST         Legacy default for an SSH cluster
#   --prefix DIR            Install prefix (default: ~/.local)
#   --state-path FILE       Private state location
#   --binary FILE           Install an existing binary instead of building
#   --force-config          Replace an existing configuration
#   Setup starts automatically on interactive installations.
#   --no-setup              Skip the interactive setup wizard
#   --no-path-update        Do not offer to add PREFIX/bin to ~/.bashrc
#   -h, --help              Show this help

set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
local_user=${USER:-$(id -un)}
remote_user=
ssh_host=
prefix=${HOME}/.local
state_path=
binary=
force_config=0
path_update=1
run_setup=1

usage() {
    sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --local-user) local_user=$2; shift 2 ;;
        --remote-user) remote_user=$2; shift 2 ;;
        --ssh-host) ssh_host=$2; shift 2 ;;
        --prefix) prefix=$2; shift 2 ;;
        --state-path) state_path=$2; shift 2 ;;
        --binary) binary=$2; shift 2 ;;
        --force-config) force_config=1; shift ;;
        --no-setup) run_setup=0; shift ;;
        --no-path-update) path_update=0; shift ;;
        -h|--help) usage; exit 0 ;;
        *) printf 'Unknown option: %s\n' "$1" >&2; usage >&2; exit 2 ;;
    esac
done

[ -n "$remote_user" ] || remote_user=$local_user

validate_identity() {
    case "$1" in
        ''|*[!A-Za-z0-9._@-]*)
            printf '%s contains unsupported characters: %s\n' "$2" "$1" >&2
            exit 2
            ;;
    esac
}
validate_identity "$local_user" "Local user"
validate_identity "$remote_user" "Remote user"
case "$ssh_host" in
    -*)
        printf 'SSH host is invalid or unsafe: %s\n' "$ssh_host" >&2
        exit 2
        ;;
    *[!A-Za-z0-9._:@%-]*)
        printf 'SSH host is invalid or unsafe: %s\n' "$ssh_host" >&2
        exit 2
        ;;
esac

config_home=${XDG_CONFIG_HOME:-${HOME}/.config}
state_home=${XDG_STATE_HOME:-${HOME}/.local/state}
config_dir=$config_home/slurm-log
config_file=$config_dir/config.json
[ -n "$state_path" ] || state_path=$state_home/slurm-log/state.json

for command in tmux; do
    if ! command -v "$command" >/dev/null 2>&1; then
        printf 'Missing runtime dependency: %s\n' "$command" >&2
        exit 1
    fi
done
if ! command -v ssh >/dev/null 2>&1; then
    printf 'Warning: ssh is not installed; local clusters still work.\n' >&2
fi
if ! command -v squeue >/dev/null 2>&1 || ! command -v scontrol >/dev/null 2>&1; then
    printf 'Warning: local squeue/scontrol not found; remote-only use may still work.\n' >&2
fi

if [ -z "$binary" ]; then
    if [ -x "$script_dir/bin/slurm-log" ]; then
        binary=$script_dir/bin/slurm-log
    else
        if ! command -v cargo >/dev/null 2>&1; then
            printf 'Rust cargo is required to build. Install Rust or pass --binary FILE.\n' >&2
            exit 1
        fi
        cargo build --locked --release --manifest-path "$script_dir/Cargo.toml"
        binary=$script_dir/target/release/slurm-log
    fi
fi
[ -x "$binary" ] || { printf 'Binary is not executable: %s\n' "$binary" >&2; exit 1; }

umask 077
mkdir -p "$prefix/bin" "$config_dir" "$(dirname -- "$state_path")"
install -m 755 "$binary" "$prefix/bin/slurm-log"

json_escape() {
    printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

if [ ! -e "$config_file" ] || [ "$force_config" -eq 1 ]; then
    local_json=$(json_escape "$local_user")
    remote_json=$(json_escape "$remote_user")
    host_json=$(json_escape "$ssh_host")
    state_json=$(json_escape "$state_path")
    config_tmp=$config_file.tmp.$$
    printf '{\n  "localUser": "%s",\n  "remoteUser": "%s",\n  "sshHost": "%s",\n  "statePath": "%s"\n}\n' \
        "$local_json" "$remote_json" "$host_json" "$state_json" >"$config_tmp"
    mv "$config_tmp" "$config_file"
else
    printf 'Keeping existing config: %s\n' "$config_file"
fi

if [ "$run_setup" -eq 1 ]; then
    if [ -t 0 ]; then
        printf '\nStarting the setup wizard...\n\n'
        "$prefix/bin/slurm-log" setup
    else
        printf '\nSetup needs a terminal and was skipped. Run: %s/bin/slurm-log setup\n' "$prefix"
    fi
fi

case ":${PATH}:" in
    *":$prefix/bin:"*) ;;
    *)
        printf '\n%s/bin is not currently on PATH.\n' "$prefix"
        if [ "$path_update" -eq 1 ] && [ -t 0 ]; then
            printf 'Add it to ~/.bashrc now? [Y/n] '
            read answer
            case "$answer" in
                n|N|no|NO) ;;
                *)
                    printf '\n# slurm-log user installation\nexport PATH="%s/bin:$PATH"\n' "$prefix" >>"$HOME/.bashrc"
                    printf 'Updated ~/.bashrc; run: source ~/.bashrc\n'
                    ;;
            esac
        else
            printf 'Add this to your shell profile: export PATH="%s/bin:$PATH"\n' "$prefix"
        fi
        ;;
esac

printf '\nInstalled: %s/bin/slurm-log\n' "$prefix"
printf 'Config:    %s\n' "$config_file"
printf 'State:     %s\n' "$state_path"
printf 'Try:       slurm-log\n'
printf 'Help:      slurm-log --help\n'
