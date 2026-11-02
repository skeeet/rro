# Build and development

Building the product requires Rust and platform linker tools. The finished executable runs
by itself. Project licensing and full dependency terms remain separate;
see [LICENSE](../LICENSE), [THIRD_PARTY_NOTICES](../THIRD_PARTY_NOTICES), and the binary
archive's dependency license bundle.

## Build the executable

Use a Rust toolchain compatible with `Cargo.toml`; its declared minimum and the compiler
actually tested are distinguished in the locally generated validation evidence. macOS builds
use Xcode Command Line Tools. Rust dependencies are locked in `Cargo.lock`; the first build
may fetch them into Cargo's external cache. No native codec build, Homebrew codec, external
encoder, or runtime download is required.

```sh
scripts/build_release.sh
./target/release/rro --version
./target/release/rro input.heic output.png --scale 0.5
```

To install the built executable for your user account:

```sh
sh scripts/install.sh target/release/rro
export PATH="$HOME/.local/bin:$PATH"
rro --version
```

Pass `--force` to replace an existing installation, or `--prefix DIR` to use
`DIR/bin/rro`. See [installation instructions](INSTALL.md) for
persistent PATH setup, archive installation, and removal. Installation only copies
the executable; it does not build or download anything.

Add `--offline` when dependencies are already cached. Use
`MACOSX_DEPLOYMENT_TARGET=13.0` for the Apple Silicon release target. A deployment target
alone does not verify execution on the oldest OS. Modify Rust sources and rebuild to obtain
a modified executable; no private signing credential or installation key is required.

## Rust checks

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps
```

## Optional independent verification and release qualification

The following tools are separate from building and running `rro`. They are required only
when regenerating the complete numerical/interoperability/performance evidence accepted by
the qualified-release packaging gate. Development checks use Python 3.11 or newer; numerical
comparisons need NumPy, image checks need Pillow, and independent HEIC checks need FFmpeg.
No such component is part of the product runtime.

Independent numerical comparison may use a hash-verified external implementation. Its
sources are never stored in this checkout or any release archive. Select an existing
external location with `RRO_REFERENCE_DIR` or `--reference-dir`; optional
`scripts/prepare_reference.py` can prepare that external location. The accepted revision
and five content hashes are recorded once in [method attribution](REFERENCE.md).
Validation refuses in-project source locations and checks hashes before importing them.

```sh
scripts/build_release.sh
python3 -B scripts/test_release_checks.py
python3 -B scripts/check_repository.py
python3 -B scripts/validate.py --parity
python3 -B scripts/core_fuzz.py
python3 -B scripts/image_validate.py
python3 -B scripts/independent_codec_check.py
python3 -B scripts/validate.py --benchmark
python3 -B scripts/benchmark_io.py
scripts/package.sh
```

A fresh source checkout contains no reports or cached timing baseline. The default benchmark
command above generates complete comparison measurements. If a complete local
`reports/numpy-baseline.json` already exists, optional `--reuse-python-baseline` avoids
expensive remeasurement only when version, source hashes, fixture geometry, and options
match. Run benchmarks while builds and other intensive jobs are idle. Reused timing baselines do not provide fresh
comparison arrays; reports preserve that limitation. `package.sh` refuses stale/incomplete
evidence for its exact executable/helper hashes, collects full dependency license texts,
checks dynamic linking, and runs the copied executable with a minimal environment.
The binary archive includes `install.sh`; the rebuild archive retains it as
`scripts/install.sh`. Installer regression checks run with the release-tooling tests.

## Documentation and repository boundaries

Human documentation starts at [README.md](../README.md). Agent instructions start at
[AGENTS.md](../AGENTS.md) and [AGENT_GUIDE.md](AGENT_GUIDE.md). Both audiences share one
[SPEC.md](SPEC.md), one stage checklist, and dated results under `reports/`; instructions
and observed results are not duplicated.

Commit Rust sources, Cargo metadata, lint policy, synthetic tests/fixtures, documentation,
reference metadata, and development tools. Reports are generated development evidence,
kept locally under ignored `reports/` and excluded from source and release archives.
Optional local `skills-lock.json` records review-tool identities and remains untracked.
Generated `target/`, `dist/`, `reports/`, bytecode, installed
local skills, and editor settings stay untracked. The binary archive contains no Python
scripts; the rebuild archive includes only the explicit development-tool allowlist.
Inventory checks reject source contamination, unexpected scripts, native build residue,
and links. Preserve attribution and licenses; never copy external research assets into
product sources.

The release wrapper remaps workspace, home, Cargo-cache, and toolchain paths before compiling,
so distributed executable diagnostics do not embed local account or machine paths. Report
generators anonymize metadata while preserving measurements, numeric errors, and source hashes.

Release archives normalize owners, numeric IDs, and timestamps; extended attributes, ACLs,
and local PAX metadata are excluded. Archive qualification checks these headers as well as
file content, paths, source hashes, and checksums.
