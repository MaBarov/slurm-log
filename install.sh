#!/bin/sh
# slurm-log portable installer
#
# Downloads or builds and installs slurm-log for the current user, writes an
# owner-scoped configuration, and checks runtime commands. It never copies
# another user's state, credentials, daemon socket, or job history.
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
#   --build                 Build this checkout instead of downloading a release
#   --version TAG           Install a release tag (default: latest)
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
build_source=0
release_version=${SLURM_LOG_VERSION:-latest}
force_config=0
path_update=1
run_setup=1
release_tmp=

cleanup() {
    [ -z "$release_tmp" ] || rm -rf "$release_tmp"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

usage() {
    sed -n '2,23p' "$0" | sed 's/^# \{0,1\}//'
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --local-user) local_user=$2; shift 2 ;;
        --remote-user) remote_user=$2; shift 2 ;;
        --ssh-host) ssh_host=$2; shift 2 ;;
        --prefix) prefix=$2; shift 2 ;;
        --state-path) state_path=$2; shift 2 ;;
        --binary) binary=$2; shift 2 ;;
        --build) build_source=1; shift ;;
        --version) release_version=$2; shift 2 ;;
        --force-config) force_config=1; shift ;;
        --no-setup) run_setup=0; shift ;;
        --no-path-update) path_update=0; shift ;;
        -h|--help) usage; exit 0 ;;
        *) printf 'Unknown option: %s\n' "$1" >&2; usage >&2; exit 2 ;;
    esac
done

case "$release_version" in
    latest) ;;
    v[0-9]*)
        case "$release_version" in
            *[!A-Za-z0-9._-]*)
                printf 'Release version is invalid: %s\n' "$release_version" >&2
                exit 2
                ;;
        esac
        ;;
    *)
        printf 'Release version must be latest or a v-prefixed tag: %s\n' "$release_version" >&2
        exit 2
        ;;
esac

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

build_release() {
    if [ ! -f "$script_dir/Cargo.toml" ] || ! command -v cargo >/dev/null 2>&1; then
        printf 'Rust/Cargo and a source checkout are required for --build.\n' >&2
        return 1
    fi
    cargo build --locked --release --manifest-path "$script_dir/Cargo.toml"
    binary=$script_dir/target/release/slurm-log
}

download_file() {
    source_url=$1
    destination=$2
    if command -v curl >/dev/null 2>&1; then
        curl -fL --retry 2 --connect-timeout 10 -o "$destination" "$source_url"
    elif command -v wget >/dev/null 2>&1; then
        wget -q --timeout=10 -O "$destination" "$source_url"
    else
        printf 'Install curl or wget to download a release.\n' >&2
        return 1
    fi
}

download_release() {
    case "$(uname -m)" in
        x86_64|amd64) architecture=x86_64 ;;
        *)
            printf 'No prebuilt release for architecture %s; trying a source build.\n' "$(uname -m)" >&2
            return 1
            ;;
    esac
    if ! command -v sha256sum >/dev/null 2>&1 || ! command -v tar >/dev/null 2>&1; then
        printf 'sha256sum and tar are required to install a prebuilt release.\n' >&2
        return 1
    fi

    release_root=${SLURM_LOG_RELEASE_ROOT:-https://github.com/MaBarov/slurm-log/releases}
    case "$release_root" in
        https://*|file://*) ;;
        *) printf 'Unsafe release URL: %s\n' "$release_root" >&2; return 2 ;;
    esac
    if [ "$release_version" = latest ]; then
        release_base=$release_root/latest/download
    else
        release_base=$release_root/download/$release_version
    fi
    asset=slurm-log-linux-$architecture.tar.gz
    release_tmp=$(mktemp -d) || return 1
    archive=$release_tmp/$asset
    checksum=$archive.sha256
    printf 'Downloading slurm-log %s for Linux %s...\n' "$release_version" "$architecture"
    if ! download_file "$release_base/$asset" "$archive" ||
       ! download_file "$release_base/$asset.sha256" "$checksum"; then
        printf 'Prebuilt release is unavailable.\n' >&2
        return 1
    fi

    expected=$(awk 'NR == 1 { print $1 }' "$checksum" | tr '[:upper:]' '[:lower:]')
    actual=$(sha256sum "$archive" | awk '{ print $1 }')
    if ! printf '%s\n' "$expected" | grep -Eq '^[0-9a-f]{64}$' || [ "$expected" != "$actual" ]; then
        printf 'Release checksum verification failed; nothing was installed.\n' >&2
        return 2
    fi
    mkdir -p "$release_tmp/payload"
    if ! tar -xzf "$archive" -C "$release_tmp/payload" slurm-log/bin/slurm-log; then
        printf 'Release archive does not contain the expected binary.\n' >&2
        return 2
    fi
    candidate=$release_tmp/payload/slurm-log/bin/slurm-log
    if [ ! -f "$candidate" ] || [ -L "$candidate" ]; then
        printf 'Release binary is missing or unsafe.\n' >&2
        return 2
    fi
    chmod 755 "$candidate"
    if ! "$candidate" --help >/dev/null 2>&1; then
        printf 'Release binary failed its startup check.\n' >&2
        return 2
    fi
    binary=$candidate
}

if [ -z "$binary" ]; then
    if [ "$build_source" -eq 1 ]; then
        build_release
    elif [ -x "$script_dir/bin/slurm-log" ]; then
        binary=$script_dir/bin/slurm-log
    elif download_release; then
        :
    else
        download_status=$?
        [ "$download_status" -ne 2 ] || exit 1
        printf 'Falling back to a locked source build.\n' >&2
        build_release
    fi
fi
[ -x "$binary" ] || { printf 'Binary is not executable: %s\n' "$binary" >&2; exit 1; }
if ! "$binary" --help >/dev/null 2>&1; then
    printf 'Binary failed its startup check: %s\n' "$binary" >&2
    exit 1
fi

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
