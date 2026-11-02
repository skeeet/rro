# Implementation stages and control gates

This is the coordination checklist for the first command-line release. Actual results are generated locally under ignored `reports/`; this file describes sequencing and ownership.

## S0 — Source audit and specification

Owner: coordination chat.

- Retain method attribution and source-identity metadata in `docs/REFERENCE.md` and `docs/reference-manifest.json`; no external research source belongs in product sources or release archives.
- Record image-only scope, CLI-only interface, MIT project license, and separate third-party notices.
- Define numerical behavior, image/color/alpha policy, memory limits, and comparison tolerances before implementation.
- Use the user-selected portable `oxideav-heif` and `oxideav-h265` stack. HEIC input and output are both required.

Gate: `docs/SPEC.md` is the current contract and the reference source hashes are recorded.

## S1 — Numerical engine

Owner: numerical implementation worker.

- Implement the specified cubic-derived camera, Gaussian, and ideal low-pass transfer functions.
- Match centered geometry, roundoff-aware ceiling, lookup tables, interpolation, padding, windows, normalization, and reduction-first execution.
- Use bounded float64 working buffers and parallel independent pixel passes.
- Test geometry/padding against independent expectations and arbitrary-length FFT against a direct transform oracle.

Gate: deterministic independent numerical agreement, maximum absolute error <= 1e-9 and RMSE <= 1e-10 before clipping. Optimization cannot weaken this gate.

## S2 — Portable image codecs

Owner: image-codec worker, with integration and independent decoder checks.

- Rust PNG/JPEG codecs and explicit color conversion.
- Pinned `oxideav-heif` and `oxideav-h265` crates for HEIC decode/encode.
- No native codec source tree, CMake dependency, external runtime codec package, Apple codec API, dynamically loaded plugin, or accidental system-codec substitution.
- Verify format round trips, orientation, color, alpha, corrupt-input handling, file-size limits, and source dimensions before pixel allocations.

Gate: both input and output work for all three formats. Known HDR inputs are either correctly tone mapped or explicitly rejected; an SDR primary image with ignored auxiliary gain map remains in scope.

## S3 — CLI integration and safe publication

Owner: CLI/validation worker.

- Wire geometry and camera controls into the executable.
- Apply premultiplied-alpha resizing and final PNG unpremultiplication or white compositing.
- Preserve input bytes, reject aliases and accidental overwrites, publish through a same-directory atomic staging file, and clean failed staging files.
- Cover paths with Unicode/spaces, invalid controls, resource rejection, and simultaneous writers.

Gate: integration tests and end-to-end PNG comparisons to independently computed expected pixels pass; HEIC/JPEG output is re-decoded and checked independently.

## S4 — Performance and regression evidence

Owners: validation worker, coordination review.

- Compare the saved independent float64 baseline with release Rust at one thread and default parallelism.
- Use 192x192, 1024x1024, and 4032x3024 arrays; cubic/Gaussian; scales 0.5, 0.75, and 1.5.
- Include kernel construction in all timings, exclude file interchange and startup from both numeric timings, warm once, and record five samples with median and p95.
- Measure peak process RSS separately and full image decode/resize/encode independently.
- Refresh final measurements after active compilation has finished to avoid avoidable CPU contention.

Gate: all parity checks pass; 1MP/12MP Rust medians do not exceed NumPy; 12MP half-scale numeric processing is below five seconds on the recorded machine; failures stay visible.

## S5 — Standalone Mac candidate

Owners: integration worker, coordination review.

- Produce a path-remapped arm64 executable, binary archive, checksums, complete notices, and source/rebuild archive; exclude development reports from both archives.
- Audit linked dynamic libraries and run copied executable in an isolated directory with a minimal environment.
- Exercise PNG input, HEIC encoding, HEIC input, and PNG output using that copied executable.
- Verify minimum deployment target metadata. Mark oldest-OS runtime, Intel, signing, and notarization as unverified when not tested.

Gate: a standalone local Mac candidate exists with explicit evidence and limitations. A successful build is not equivalent to a notarized public release.

## S6 — Other platform release qualification

Owner: future native-platform verification runs.

- Execute Windows and Linux build/test/lint jobs on those systems.
- Exercise every codec, resource rejection, path behavior, and no-clobber publication there.
- Inspect platform dynamic links and package standalone platform-specific executables.

Gate: actual recorded native-platform runs and artifacts. Shared portable code alone does not satisfy this gate.
