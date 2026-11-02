#!/bin/sh
# Install an already-built executable without downloads or profile changes.
set -eu

usage() {
    cat <<'EOF'
Usage: install.sh [--prefix DIR] [--force] [BINARY]

Install rro to DIR/bin/rro (default: $HOME/.local/bin/rro).
Use --force to replace an existing file or symbolic link.

Without BINARY, look beside this installer for rro, then in
../dist/rro and ../target/release/rro, in that order.
No build, download, privilege escalation, or shell-profile edit is performed.
EOF
}

fail() {
    printf 'Install failed: %s\n' "$*" >&2
    exit 1
}

prefix=
prefix_set=false
force=false
source_binary=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --help|-h) usage; exit 0 ;;
        --force) force=true; shift ;;
        --prefix)
            [ "$#" -ge 2 ] || fail '--prefix requires a directory.'
            [ -n "$2" ] || fail '--prefix requires a nonempty directory.'
            case "$2" in -*) fail '--prefix requires a directory; prefix a relative path beginning with - with ./.' ;; esac
            prefix=$2
            prefix_set=true
            shift 2
            ;;
        --)
            shift
            [ "$#" -le 1 ] || fail 'Only one binary path is accepted.'
            if [ "$#" -eq 1 ]; then
                [ -z "$source_binary" ] || fail 'Only one binary path is accepted.'
                source_binary=$1
                [ -n "$source_binary" ] || fail 'The binary path must not be empty.'
                shift
            fi
            ;;
        -*) fail "Unknown option: $1. Use --help for usage." ;;
        *)
            [ -z "$source_binary" ] || fail 'Only one binary path is accepted.'
            source_binary=$1
            [ -n "$source_binary" ] || fail 'The binary path must not be empty.'
            shift
            ;;
    esac
done

if [ "$prefix_set" = false ]; then
    [ -n "${HOME-}" ] || fail 'HOME is unset; provide --prefix DIR.'
    prefix=$HOME/.local
fi
case "$prefix" in
    /*) ;;
    *) prefix=$PWD/$prefix ;;
esac

if [ -z "$source_binary" ]; then
    script_directory=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd -P)
    for candidate in "$script_directory/rro" "$script_directory/../dist/rro" "$script_directory/../target/release/rro"; do
        if [ -f "$candidate" ]; then
            source_binary=$candidate
            break
        fi
    done
    [ -n "$source_binary" ] || fail 'No executable found. Extract a binary release or build rro, then pass its path.'
fi
case "$source_binary" in
    /*) ;;
    *) source_binary=$PWD/$source_binary ;;
esac
[ -f "$source_binary" ] && [ -s "$source_binary" ] && [ -r "$source_binary" ] ||
    fail "Binary is not a readable, nonempty regular file: $source_binary"

bin_directory=$prefix/bin
destination=$bin_directory/rro
[ ! -d "$destination" ] || fail "Destination is a directory: $destination"
if [ -e "$destination" ] || [ -L "$destination" ]; then
    [ ! "$source_binary" -ef "$destination" ] || fail 'Source and destination refer to the same file.'
    [ "$force" = true ] || fail "Destination exists: $destination. Use --force to replace it."
fi
mkdir -p -- "$bin_directory" || fail "Cannot create installation directory: $bin_directory"

# Stage on the destination filesystem so publication cannot expose a partial copy.
staging_directory=$(mktemp -d "$bin_directory/.rro-install.XXXXXX") ||
    fail "Cannot stage installation in: $bin_directory"
trap 'rm -rf -- "$staging_directory"' 0
trap 'exit 1' HUP INT TERM
cp -- "$source_binary" "$staging_directory/rro"
chmod 755 "$staging_directory/rro"
if [ "$force" = true ]; then
    # Rename replaces a symbolic link entry rather than writing through it.
    [ ! -d "$destination" ] || fail "Destination is a directory: $destination"
    mv -f -- "$staging_directory/rro" "$destination"
else
    # A hard link publishes a complete file and refuses a concurrent file creation.
    ln -n -- "$staging_directory/rro" "$destination" ||
        fail "Cannot publish $destination. Destination may already exist; use --force to replace it."
fi
printf 'Installed %s\n' "$destination"
printf '%s\n' 'Run the installed executable with --version to verify it.'
case ":${PATH-}:" in
    *":$bin_directory:"*) ;;
    *)
        printf 'Add this directory to PATH in your shell profile: %s\n' "$bin_directory"
        printf '%s\n' 'For the default prefix, use: export PATH="$HOME/.local/bin:$PATH"'
        ;;
esac
