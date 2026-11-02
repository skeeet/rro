# ResizeRight Optics CLI: macOS first, portable image I/O

Status: normative implementation specification, 2026-10-27. This document defines requirements for automation and implementation agents, not measured results. The product is a standalone Rust command-line executable for PNG/JPEG/HEIC still images. Portable HEIC decoding and encoding use the user-selected `oxideav-heif` and `oxideav-h265` crates. No GUI is planned.

## 1. Product and boundary

Build `rro`, a native macOS executable that decodes one image, applies the camera-consistent spatial resampling mathematics from ResizeRightOptics, and writes one image. Copying the executable to a supported Mac must be sufficient to run it. A Python interpreter, virtual environment, Homebrew, runtime downloads, and network access must not be required.

The first executable targets Apple Silicon macOS 13 or newer. This machine is arm64 macOS 26.6.2; runtime verification here does not establish compatibility on macOS 13. An Intel build and universal packaging are later release work requiring actual cross-build/runtime verification. The core, CLI, and codec stack must be portable to Windows and Linux, with no required Apple codec APIs. Shipping those platforms requires separate compile/runtime tests and platform release artifacts; portability of dependencies is not proof of a tested release. Build tools are development dependencies only. Platform system libraries are allowed at runtime; codec crates are compiled into the executable.

Initial input and output extensions: `.png`, `.jpg`, `.jpeg`, `.heic`, `.heif`. HEIF container input must actually contain a supported HEVC-coded still image. Unsupported content and containers must fail clearly, even when renamed to an accepted extension. Process the primary still image only. Multi-frame animation, Live Photo video, RAW, HDR preservation, depth maps, auxiliary HEIF images, and other formats are out of scope.

No tensor API, automatic differentiation, GPU framework, model training, arbitrary-dimensional resampling, paper experiments, Python packaging, or research dataset management is required. No claim of image-quality superiority follows from matching the implementation.

## 2. Numerical scope

Method attribution and source-identity metadata are centralized in
`docs/REFERENCE.md` and `docs/reference-manifest.json`; licensing terms are retained in
`THIRD_PARTY_NOTICES`. No external research implementation is part of the product or build.
The default camera is inferred from Keys cubic at construction scale 0.5 using a
24-term spectral product and 24-point Gauss-Legendre integration per polynomial
segment. This is not ordinary bicubic interpolation.

First-version camera choices:

| Camera | Parameter | Default |
| --- | --- | --- |
| `cubic` | Fixed camera construction scale 0.5 and 24 terms | Product default |
| `gaussian` | Positive sigma in source-pixel units | 0.7 when selected |
| `ideal-lowpass` | Positive cutoff in radians per pixel | pi when selected |

Only the separable finite spatial path is shipped initially. Nonseparable Airy/covariance models, sampled/custom PSFs, Fourier image resizing, and the separate polyphase execution path are explicitly deferred. Rational phase reuse or FFT implementation optimizations may be added only after equivalence passes.

Required numerical behavior:

- Preserve explicit intended scale even when output-size rounding yields a different size ratio.
- Project output centers by `(j - (m-1)/2)/s + (n-1)/2`.
- Infer missing dimensions by `max(1, ceil(n*s - 8*ulp(n*s)))`, with a floating-point boundary guard.
- Infer scales from explicit output dimensions only when scales were omitted.
- Use the source/target digital Nyquist bands and the relative cutoff threshold, camera transfer `H(w/s)*conj(H(w))/(abs(H(w))^2 + eta^2)`.
- Construct the finite kernel with midpoint Fourier quadrature, the phase correction, and cubic Keys table interpolation by default.
- Use support, density, period factor, normalization, and window defaults: 32, 8, 4, enabled, Hann. Expose linear/cubic table interpolation and none/Hann/Lanczos windows.
- Match reflect, symmetric, edge, wrap, and zero-constant padding, including singleton axes and supports larger than the image. Normalize phase weights before excluding zero-padding contributions.
- Execute the axis with the smaller scale first; ties preserve height-before-width order.
- Work in float64 for the initial CPU core. Do not clamp intermediate values or replace the camera ratio with a standard resizing kernel. Clip only when converting the final image to integer pixels.
- Warn when computed inverse gain exceeds 1000. Negative or non-finite controls must fail.

## 3. Command-line contract

Examples:

```sh
rro input.heic output.png --scale 0.5
rro input.jpg output.heic --width 1600
rro input.png output.jpg --width 1600 --height 900 --camera gaussian --sigma 0.7
rro input.png output.png --scale 0.5 --width 601 --height 401
```

Two positional paths are required for conversion. Exactly one of scalar `--scale`, a pair `--scale-x/--scale-y`, or dimensions can establish geometry. Explicit scale plus both dimensions is allowed and retains that scale. A single width or height without scale preserves aspect ratio and uses guarded ceiling sizing on the other axis. Both dimensions without scale permit aspect distortion. A partial explicit dimension combined with scale, or incomplete per-axis scales, must fail to avoid guessing. Width/X means columns; height/Y means rows. Zero, NaN, infinite, and unrepresentable dimensions/scales fail.

Additional controls: `--camera`, `--sigma`, `--cutoff`, `--regularization` (default 0), `--rcond` (1e-12), `--support` (32), `--density` (8), `--period-factor` (4), `--padding` (reflect), `--window` (hann), `--table-interp` (cubic), `--no-normalize`, `--quality` (0.9 for JPEG/HEIC), `--threads` (default machine parallelism), `--max-memory-mib` (2048), `--force`, and `--timings`.

Reject parameters that do not apply to the selected camera instead of silently ignoring them. Standard output stays empty on success unless explicitly requested; concise status, timing, and warnings go to standard error. Help and version go to standard output. Nonzero exit means failure. Paths with spaces and Unicode are supported. Reject embedded NUL safely at the native boundary.

Never overwrite an existing destination unless `--force`. Reject input/output aliases, including symlinks and hard links, even with `--force`. Encode into a same-directory temporary file, then publish atomically. Existing output remains intact on failure. Handle the no-clobber race, not just an initial existence check. Never alter the input. No directory batch mode in the first version.

## 4. Image contract

Preserve source alpha-channel presence independently of pixel opacity. PNG output
from an input without alpha must be RGB, with no added alpha channel. An input
with alpha must produce RGBA PNG even when every alpha value is opaque. PNG
transparency metadata (`tRNS`) counts as alpha; JPEG has no alpha, and HEIF alpha
comes from its associated alpha image. Internal RGBA decode buffers must retain
this distinction. Resize three color channels for inputs without alpha and four
premultiplied channels for inputs with alpha; padding must not invent transparency
for an input without alpha. Color conversion and orientation preserve this flag.

Use the portable Rust `image` crate with only PNG/JPEG features enabled. Use `moxcms` for explicit ICC conversion to sRGB. Per the user's explicit selection, HEIC encoding and decoding use `oxideav-heif` for the container and `oxideav-h265` for HEVC. Prefer the high-level container API for correct grids, alpha, transforms, and limits; document any additional codec crates forced by its registry feature. The product must reject non-HEVC image payloads even if transitive framework dependencies could decode them. Inspect actual pinned source/API coverage, not just a crate description: the requested release must support ordinary lossy still-image encoding as well as decoding. Prefer published versions when they cover the required API; otherwise pin exact compatible Git revisions in the lockfile and record why. No native libheif, libde265, Kvazaar, CMake, Apple image codec, dynamically loaded codec, or external codec executable is part of the final application. Preserve dependency license texts separately from the project's MIT license.

Decode to standard sRGB 8-bit straight RGBA, apply all eight EXIF orientations exactly once, and deliver top-left-origin rows. Respect HEIF container crop/rotation/mirror transforms and avoid double application of equivalent EXIF rotation. ICC conversion must be explicit; reading a profile is not conversion. Handle supported RGB/grayscale profiles; reject unsupported profile spaces or transfer functions clearly rather than relabeling unconverted pixels as sRGB. High-bit-depth SDR may be quantized to 8 bits. HDR/PQ/HLG conversion requires a verified tone mapper; until implemented, reject explicit HDR encodings clearly rather than merely right-shifting them. Apple HDR gain maps may be ignored when a valid SDR primary image is provided. Initial release does not promise original ICC/profile/metadata preservation.

Resize encoded sRGB channel values, using normalized image arrays. Do not silently linearize. For alpha-bearing inputs, premultiply RGB by alpha before the core resize, resize all four channels, and unpremultiply afterward; pixels with zero final alpha get zero RGB. This avoids invisible-color edge halos. Pixel parity tests must distinguish direct core channel parity from this product alpha wrapper. PNG preserves alpha. JPEG/HEIC composite onto white; document this fixed first-version background. Quality is 0 through 1. HEIC encoding must succeed through the statically bundled software encoder and fail clearly on an encoding error; it must not depend on an installed system encoder.

Outputs are 8-bit SDR images with normalized orientation and sRGB intent. Do not copy GPS, EXIF thumbnails, camera identifiers, or stale orientation tags. Lossy codec bytes and quality settings are not interchangeable between encoders; compare decoded pixels using format-specific criteria. Image input/output support means both decode and encode, not just extension acceptance.

## 5. Architecture and implementation boundaries

One Cargo package with library and CLI, minimal dependencies. Lock resolved Rust dependencies, including exact Git revisions if needed. Keep the numeric core independent from codecs and command-line parsing. Dependency sources remain in Cargo's external cache; do not vendor abandoned native codec sources into this repository. Nothing is downloaded or built when the user runs the finished executable. External research implementations must not be present in the repository or release/rebuild archives. Preserve attribution, license terms, and source hashes in `docs/REFERENCE.md`, `docs/reference-manifest.json`, and `THIRD_PARTY_NOTICES`.

| Component | Files | Responsibility |
| --- | --- | --- |
| Numerical core | `src/core.rs` | Geometry, cameras, kernel tables, boundary weights, parallel separable passes, allocation checks |
| Portable image I/O | `src/codec.rs` and private Rust codec helpers | Rust PNG/JPEG, OxideAV HEIF/HEVC decode/encode, ICC/color/orientation, bounded buffers, error translation |
| CLI and product pipeline | `src/main.rs`, `src/lib.rs`, `Cargo.toml` | Controls, alpha policy, path safety, atomic publish, timing |
| Development numerical checks | `scripts/validate.py`, `src/bin/rro-numeric.rs` | Development-only raw float interchange, parity matrix, benchmark reports |
| Release | `scripts/package.sh`, `README.md`, `reports/` | Standalone artifact, runtime link audit, reproducibility and evidence |

Shared worker interface, defined before implementation:

```rust
// src/core.rs
pub struct Image { pub width: usize, pub height: usize, pub channels: usize, pub data: Vec<f64> }
pub enum Camera { Cubic, Gaussian { sigma: f64 }, IdealLowpass { cutoff: f64 } }
pub enum Padding { Reflect, Symmetric, Edge, Wrap, Constant }
pub enum Window { Hann, Lanczos, None }
pub enum TableInterp { Cubic, Linear }
pub struct ResizeOptions {
    pub scale_x: Option<f64>, pub scale_y: Option<f64>,
    pub width: Option<usize>, pub height: Option<usize>, pub camera: Camera,
    pub support: f64, pub density: f64, pub period_factor: f64,
    pub regularization: f64, pub rcond: f64, pub padding: Padding,
    pub window: Window, pub table_interp: TableInterp, pub normalize: bool,
    pub max_memory_bytes: usize,
}
impl Default for ResizeOptions { /* specified defaults; no geometry */ }
pub fn resize(input: &Image, options: &ResizeOptions) -> Result<Image, ResizeError>;
// Rayon thread pool is controlled by the CLI, not mutable global core state.
// ResizeError implements Error + Display and preserves allocation causes.

// src/codec.rs
pub enum AlphaChannel { Absent, Present }
pub struct RgbaImage { pub width: usize, pub height: usize, pub pixels: Vec<u8>, pub alpha: AlphaChannel }
pub enum OutputFormat { Png, Jpeg, Heic }
pub fn decode(path: &std::path::Path, max_pixels: usize) -> Result<RgbaImage, CodecError>;
pub fn encode(image: &RgbaImage, path: &std::path::Path, format: OutputFormat, quality: f64) -> Result<(), CodecError>;
// CodecError implements Error + Display and preserves underlying codec/I/O causes.
// The output path extension is not authoritative inside the codec API.
```

The raw numeric helper consumes JSON options/dimensions plus little-endian float64 pixel files. It never participates in normal image conversion. Exact helper schema is owned by the validation worker and documented with its script.

Follow the installed Rust skills for code and review. The companion library uses documented typed errors and standard logging; the CLI adds path context with one application error mechanism and controls stderr diagnostics. Keep narrowly justified mathematical conversions explicit, preserve exact generated quadrature, and require formatting, strict configured Clippy, tests, and rustdoc checks before release. `docs/PORTING.md` describes the numerical behavior and architecture; locally generated audit reports record prioritized findings and resolutions. Development reports remain ignored and excluded from source/release archives.

Validate dimensions and all size products before allocation, including source decode, float conversion, pass intermediates, neighbor maps, and kernel tables. Memory accounting must bound estimated live buffers and reject over-budget work early; document that allocator/framework overhead makes the process RSS different from this working-buffer budget. Use checked arithmetic and fallible allocation where feasible. Do not materialize large neighborhood tensors. Kernel table cap: 1,048,576 samples per axis. Reject unreasonable supports rather than spending unbounded time. Finite output values are required before encoding.

## 6. Stages and gates

| Stage | Deliverable | Exit gate |
| --- | --- | --- |
| S0: audit/spec | Provenance metadata, scope, interfaces, stage plan | Spec exists before implementation; no research assets shipped |
| S1: numeric port | Three camera families, geometry, table construction, separable core | Unit tests cover independent geometry/padding oracles, invalid inputs, memory bounds; parity harness passes |
| S2: file I/O | Portable PNG/JPEG/HEIC decoding/encoding using the selected Rust crates | All three formats round-trip; orientation/color/alpha tests pass; corrupt/unsupported inputs fail; OxideAV HEVC output is checked with an independent decoder; no native codec dependency |
| S3: CLI integration | Standalone conversion workflow and safety | Documented commands work; input unchanged; overwrite/alias/race/failure checks pass |
| S4: parity/performance | Machine-readable reports and benchmark protocol | Numeric tolerances pass; performance measured fairly with no relaxed parity |
| S5: release candidate | Optimized arm64 executable, checksum, licenses, usage | Binary runs from isolated directory with minimal PATH; dynamic dependencies are Apple system libraries only; limitations and unverified platforms explicit |

S1 and S2 may proceed concurrently after S0. CLI/validation work may proceed against the fixed interfaces. This chat coordinates and reviews; bounded subagents implement specific components. A worker must report code, tests, and limitations without claiming another worker's gate is satisfied.

## 7. Numerical and codec acceptance

Use identical normalized arrays, exact intended scales, output sizes, camera controls,
padding, table settings, and phase normalization for independent numerical comparisons.
Compare values before clipping, alpha processing, or encoding. Seed synthetic data
deterministically and include random RGB/RGBA, constant, impulse, ramp, checkerboard,
sinusoidal and signed/high-dynamic-range inputs. Include singleton dimensions, odd/even
transitions, severe reductions, enlargement, irrational and anisotropic scales, explicit
scale plus differing output ratios, unit-scale crops, all padding/window/table modes,
and regularization. Optional external comparison setup belongs in `docs/REBUILD.md`;
it is separate from building or running the Rust product.

Acceptance for bounded/conditioned float64 fixtures: maximum absolute error <= 1e-9, RMSE <= 1e-10. For large inverse gains, report instability separately and use explicit regularization; do not hide discrepancies through final clipping. Record worst case, settings, fixture size, comparison-source identity, machine, compiler, dependency versions, and counts. Relative tolerances may be added for deliberately large signal magnitudes only with the absolute error still reported and justified.

Use exact lossless round-trips for opaque synthetic PNG pixels when scale=1. Compare normal resize PNG output with independently rounded/clipped independent expected pixels: maximum 1 code-value difference (rounding ties allowed), with fractions of differing pixels recorded. Test all eight orientation transforms using asymmetric independent expected pixels. Check transparency edges against premultiplied-alpha expectation. JPEG/HEIC tests require successful output re-decode, correct dimensions/orientation, opaque output, and synthetic smooth-patch PSNR >= 30 dB at quality 0.9; report exact metrics rather than expecting byte equality. Include unusual paths and truncated files.

## 8. Performance protocol and release targets

Use `scripts/build_release.sh` with dynamic local-path remapping. Record arm64 hardware/OS, logical cores, compiler, Python/NumPy versions, thread counts, algorithm settings, and revision. Never compare debug Rust to release Python or exclude kernel construction on only one side.

Separate (a) numeric resize including kernel construction and (b) full process decode/resize/encode. Warm once, then measure at least five iterations; report median and p95, not just the best sample. Python process startup is excluded from numeric timings; Rust process startup must also be excluded or explicitly reported as an upper bound. The helper should perform repeated operations in one process. Time the same float64 arrays and both default cubic and Gaussian 0.7 at 0.5, 0.75, 1.5 scales. Run 192x192, 1024x1024 and a 4032x3024 photo-shaped array where memory permits. Include one-thread Rust and default parallel Rust, and record reference thread environment. Record peak RSS in a separate run, accounting for differing OS units. Do not silently skip resource failures; report them.

Initial acceptance targets, subject to measured feasibility: on this machine, release Rust numeric median <= NumPy median for 1MP and 12MP Gaussian/cubic, and 12MP half-scale completes within 5 seconds with working buffers below 2 GiB. At small sizes report kernel/setup overhead honestly. These are targets, not established facts; an unmet target keeps S4 incomplete and requires a documented optimization stage. Output parity must pass again after each numerical optimization.

Likely safe optimization sequence: avoid full neighborhood tensors, precompute each axis's weights, reuse identical kernel tables within one call, parallelize independent output rows, then consider phase reuse/SIMD only when profiles justify it. No persistent camera cache required initially.

## 9. Release evidence and open limitations

The reimplementation is MIT-licensed, as explicitly requested by the user. The root `LICENSE` names only this project's contributors. Method attribution and original terms are preserved separately in `THIRD_PARTY_NOTICES`, also included with release archives. External research implementation sources are excluded from the repository and all archives. Preserve Rust dependency license texts separately. Ship executable plus README, project license, and third-party notices in a small archive; no Python source or build tree required by users. A companion source archive contains this implementation and build metadata, with no external research implementation or abandoned codec code. Record SHA-256 and `otool -L` output. Ad-hoc signing may be used for local integrity; Developer ID signing/notarization requires user credentials and is not assumed. Download quarantine/Gatekeeper behavior must be described honestly; no claim of notarization, Intel, Windows, or Linux validation without evidence.

`reports/VALIDATION.md` summarizes passed, failed, deferred, and unverified checks with actual counts/timings. `reports/parity.json` and `reports/benchmark.json` preserve raw results. Future work includes other formats, wider bit depth/HDR/color controls, batch conversion, Airy/custom cameras, and Intel/universal support; none is silently implied by the first release.
