#!/usr/bin/env bash
# Build host release executables without embedding developer directory names.
set -euo pipefail

script_location=${BASH_SOURCE[0]}
case "$script_location" in
    */*) script_directory=${script_location%/*} ;;
    *) script_directory=. ;;
esac
workspace_directory=$(CDPATH= cd -- "$script_directory/.." && pwd -P)
cd -- "$workspace_directory"

# Cargo gives encoded flags precedence over RUSTFLAGS. Preserve that precedence
# and Cargo's whitespace splitting when converting an ordinary RUSTFLAGS value.
flag_separator=$'\x1f'
encoded_flags=${CARGO_ENCODED_RUSTFLAGS-}
if [[ ! ${CARGO_ENCODED_RUSTFLAGS+x} && -n ${RUSTFLAGS-} ]]; then
    inherited_flags=()
    IFS=$' \t\n' read -r -d '' -a inherited_flags <<< "$RUSTFLAGS" || true
    # Bash 3.2 treats an empty array as unset under nounset.
    for flag in ${inherited_flags[@]+"${inherited_flags[@]}"}; do
        encoded_flags+="${encoded_flags:+$flag_separator}$flag"
    done
fi

append_remap() {
    local source_directory=$1
    local replacement_directory=$2
    if [[ -z "$source_directory" ]]; then
        return
    fi
    if [[ "$source_directory" == *"$flag_separator"* ]]; then
        printf '%s\n' 'Cannot remap a directory containing the Cargo flag separator.' >&2
        exit 1
    fi
    if [[ "$source_directory" != /* && "$source_directory" != [A-Za-z]:* ]]; then
        source_directory="$workspace_directory/$source_directory"
    fi
    source_directory=${source_directory%/}
    encoded_flags+="${encoded_flags:+$flag_separator}--remap-path-prefix=$source_directory=$replacement_directory"
    # Resolve directory aliases too: compiler paths may use the physical cache
    # location rather than the spelling of HOME or CARGO_HOME supplied by users.
    if [[ -d "$source_directory" ]]; then
        local physical_directory
        physical_directory=$(CDPATH= cd -- "$source_directory" && pwd -P)
        if [[ "$physical_directory" != "$source_directory" ]]; then
            encoded_flags+="$flag_separator--remap-path-prefix=$physical_directory=$replacement_directory"
        fi
    fi
}

# rustc applies the last matching prefix. Append broad home remaps first and
# the project remap last, after caller flags, so nested paths remain anonymous.
append_remap "${HOME-}" /build/home
cargo_directory=${CARGO_HOME-${HOME:+$HOME/.cargo}}
append_remap "$cargo_directory" /build/cargo
rustc_command=${RUSTC:-rustc}
toolchain_directory=$("$rustc_command" --print sysroot)
append_remap "$toolchain_directory" /build/rust
append_remap "$workspace_directory" /src/rro
export CARGO_ENCODED_RUSTFLAGS=$encoded_flags

if [[ $(uname -s) == Darwin ]]; then
    export MACOSX_DEPLOYMENT_TARGET=${MACOSX_DEPLOYMENT_TARGET:-13.0}
fi

exec cargo build --release --locked --bins "$@"
