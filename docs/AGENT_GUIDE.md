# Agent documentation

Audience: implementation agents, audit workers, and release automation. Human usage and
build explanations are in [README.md](../README.md).

## Authority and routing

1. [../AGENTS.md](../AGENTS.md) defines workspace rules and coordination boundaries.
2. [SPEC.md](SPEC.md) is the normative behavior and acceptance contract. Never treat acceptance targets as test outcomes.
3. [STAGES.md](STAGES.md) defines execution gates and ownership.
4. [release-contract.json](release-contract.json) records the exact expected fixture IDs,
   numeric tolerances, benchmark matrix, and performance targets checked by
   `scripts/check_release.py`. A deliberate suite change must update the runner, contract,
   and tests together. Never shrink a suite or relax a tolerance to pass a release.
5. Actual outcomes, artifact hashes, and audit findings are generated locally under ignored
   `reports/`. They are development evidence, never committed source or archive payload.

## Execution protocol

The parent chat coordinates, specifies, reviews, and integrates. Bounded workers own
explicit nonoverlapping files. Preserve other workers' edits. No external chats or messages.
Read the complete specification before editing. Run the configured Rust checks plus
`scripts/test_release_checks.py` and `scripts/check_repository.py`. No blanket lint weakening.

After all Rust edits stop, build both release executables with
`scripts/build_release.sh`. Freeze those artifacts
while refreshing deterministic parity, randomized parity, image pipeline checks, independent
HEIC interoperability, numeric performance, and I/O performance. Use a quiet CPU window for
benchmarks. A fresh checkout has no timing baseline; generate full measurements by default. Reuse an
existing local `reports/numpy-baseline.json` only when its comparison-source hashes, NumPy version,
fixture geometry, and options match. Reused timing baselines do not establish fresh output
parity; the benchmark report must keep that distinction visible.

`package.sh` rejects incomplete or stale evidence, checks source inventories, gathers full
license texts, runs the staged executable with a minimal environment, and writes archives
without macOS AppleDouble resource sidecars. `audit_archives.py` verifies archive paths,
entry types, source inventory, and checksum consistency without extracting untrusted paths.
Run both after edits to packaged documentation, scripts, notices, or Rust sources.

## Generated files and source boundaries

Optional independent comparison setup belongs only in `docs/REBUILD.md`. The product and
normal Rust build do not require any external research implementation. Attribution and
source-identity metadata are centralized in `docs/REFERENCE.md` and
`docs/reference-manifest.json`.

All generated outputs stay inside this workspace: Cargo outputs in `target/`, artifacts in
`dist/`, development evidence and temporary verification data in `reports/`. Never use `/tmp`. These paths
are ignored by Git. Keep reference source hashes, notices, synthetic test fixtures, Cargo.lock, and lint
configuration in source control. Keep reports and optional local `skills-lock.json`
review-tool identities untracked.
Local skill installations and editor settings remain available but untracked.

Project-authored `.py` files under the explicit `scripts/` allowlist are development tools,
not a second product implementation. No Python resizer package, virtual environment,
vendored native codec, research asset, or obsolete backend is allowed. The finished `rro`
executable must run with no Python, Homebrew codec, external codec program, or network.

## Claims requiring separate qualification

Do not infer Windows/Linux releases, Intel support, macOS 13 runtime compatibility, MSRV
compatibility, signing/notarization, unsupported research API coverage, or hard RSS containment from
the current Mac build. Preserve explicit unverified status until actual platform/toolchain
runs establish it. A dated advisory query reports known database matches only; it is not
proof that third-party decoder code contains no vulnerabilities.

## Anonymous artifacts

Release builds use `scripts/build_release.sh` for dynamic path remapping. Report writers
use `scripts/report_privacy.py`; preserve errors, tolerances, timings, and source hashes.
Inventory and archive checks reject private account/machine paths and local identities.
Use generic project author and committer identity for generated local commits without
changing the user's global Git configuration. Preserve required third-party attribution.
After final package validation, remove `target/`, `reports/work/`, and bytecode; preserve
final `dist/` artifacts, local compact evidence, and user-owned IDE/skill installations.
