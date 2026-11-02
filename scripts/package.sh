#!/bin/sh
# Build and package one native executable plus a companion rebuild-source kit.
set -eu
export COPYFILE_DISABLE=1
export PYTHONDONTWRITEBYTECODE=1
cd "$(dirname "$0")/.."
ROOT=$(pwd)
PLATFORM=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)
VERSION=0.1.0
NAME="rro-$VERSION-$PLATFORM-$ARCH"
if [ "$PLATFORM" = darwin ]; then
    export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-13.0}"
fi
scripts/build_release.sh --offline
python3 scripts/test_release_checks.py
python3 scripts/check_repository.py
python3 scripts/check_release.py
mkdir -p dist reports/work
PACK_ROOT=$(mktemp -d "$ROOT/reports/work/package.XXXXXX")
trap 'rm -rf "$PACK_ROOT"' EXIT HUP INT TERM
STAGE="$PACK_ROOT/$NAME"
SOURCE_STAGE="$PACK_ROOT/$NAME-rebuild-source"
mkdir -p "$STAGE/licenses" "$SOURCE_STAGE"
cp target/release/rro "$STAGE/rro"
cp scripts/install.sh "$STAGE/install.sh"
cp LICENSE THIRD_PARTY_NOTICES README.md AGENTS.md "$STAGE/"
cp -R docs "$STAGE/docs"
python3 scripts/collect_licenses.py "$STAGE/licenses"
if [ "$PLATFORM" = darwin ]; then
    (cd "$STAGE" && otool -L rro) > "$STAGE/DYNAMIC-LINKS.txt"
    if tail -n +2 "$STAGE/DYNAMIC-LINKS.txt" | awk '{print $1}' | grep -Ev '^(/usr/lib/|/System/Library/)' >/dev/null; then
        echo 'Release rejected: executable links to a non-system shared library.' >&2
        exit 1
    fi
    (cd "$STAGE" && otool -l rro) > "$STAGE/MACHO-LOAD-COMMANDS.txt"
fi
(cd "$STAGE" && file rro) > "$STAGE/BINARY-TYPE.txt"
rustc --version > "$STAGE/BUILD.txt"
printf 'Platform: %s\nArchitecture: %s\n' "$PLATFORM" "$ARCH" >> "$STAGE/BUILD.txt"
if [ "$PLATFORM" = darwin ]; then
    printf 'MACOSX_DEPLOYMENT_TARGET: %s\n' "$MACOSX_DEPLOYMENT_TARGET" >> "$STAGE/BUILD.txt"
fi
# Exercise a copied executable with no developer-tool or package-manager PATH.
(cd "$STAGE" && env -i PATH=/usr/bin:/bin ./rro --version) > "$STAGE/ISOLATED-SMOKE.txt"
FIXTURE="$ROOT/reports/work/image-oracle/opaque-input.png"
if [ -f "$FIXTURE" ]; then
    cp "$FIXTURE" "$STAGE/smoke-input.png"
    (cd "$STAGE" && env -i PATH=/usr/bin:/bin ./rro smoke-input.png smoke-output.png --scale 0.5 --timings) >> "$STAGE/ISOLATED-SMOKE.txt" 2>&1
    (cd "$STAGE" && env -i PATH=/usr/bin:/bin ./rro smoke-input.png smoke-output.heic --scale 0.5 --quality 0.9 --timings) >> "$STAGE/ISOLATED-SMOKE.txt" 2>&1
    (cd "$STAGE" && env -i PATH=/usr/bin:/bin ./rro smoke-output.heic smoke-heic-decoded.png --scale 1 --timings) >> "$STAGE/ISOLATED-SMOKE.txt" 2>&1
    rm "$STAGE/smoke-input.png" "$STAGE/smoke-output.png" "$STAGE/smoke-output.heic" "$STAGE/smoke-heic-decoded.png"
else
    echo 'No image fixture found. Run scripts/image_validate.py before packaging.' >&2
    exit 1
fi
# The companion source kit contains this Rust product and locked dependencies, not
# external research implementations, generated outputs or
# Git data. Metadata and external-reference preparation scripts are included.
cp Cargo.toml Cargo.lock clippy.toml AGENTS.md .gitignore LICENSE THIRD_PARTY_NOTICES README.md "$SOURCE_STAGE/"
cp -R src docs tests "$SOURCE_STAGE/"
mkdir -p "$SOURCE_STAGE/scripts"
cp scripts/package.sh scripts/validate.py scripts/image_validate.py scripts/collect_licenses.py scripts/prepare_reference.py scripts/check_release.py scripts/benchmark_io.py scripts/independent_codec_check.py "$SOURCE_STAGE/scripts/"
if [ -f scripts/core_fuzz.py ]; then cp scripts/core_fuzz.py "$SOURCE_STAGE/scripts/"; fi
cp scripts/test_release_checks.py scripts/check_repository.py scripts/audit_archives.py scripts/report_privacy.py scripts/create_archive.py scripts/build_release.sh "$SOURCE_STAGE/scripts/"
cp scripts/install.sh "$SOURCE_STAGE/scripts/"
python3 scripts/check_repository.py --staged-directory "$SOURCE_STAGE"
python3 scripts/create_archive.py "$STAGE" "dist/$NAME.tar.gz"
python3 scripts/create_archive.py "$SOURCE_STAGE" "dist/$NAME-rebuild-source.tar.gz"
cp "$STAGE/rro" dist/rro
shasum -a 256 dist/rro "dist/$NAME.tar.gz" "dist/$NAME-rebuild-source.tar.gz" > dist/SHA256SUMS
if [ -f "$STAGE/DYNAMIC-LINKS.txt" ]; then cp "$STAGE/DYNAMIC-LINKS.txt" reports/dynamic-links.txt; fi
cp "$STAGE/ISOLATED-SMOKE.txt" reports/isolated-smoke.txt
python3 scripts/audit_archives.py
printf 'Packaged %s\n' "dist/$NAME.tar.gz"
cat dist/SHA256SUMS
