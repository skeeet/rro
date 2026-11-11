# Numerical and architecture notes

`rro` is a Rust image-file conversion application with a small public numerical and codec
library. It implements the separable finite spatial camera-derived method credited in
[method attribution](REFERENCE.md). Public interfaces use typed errors and documented image
and options records. The command line adds file paths and diagnostic context.

## Numerical behavior

- Images use float64 HWC storage. Output dimensions and allocation products are checked.
- Output centers are `(j - (m-1)/2)/s + (n-1)/2`. Requested scales are retained when explicit
  output dimensions differ from their inferred ratio.
- Scale-derived dimensions use a guarded ceiling: `max(1, ceil(n*s - 8*ulp(n*s)))`.
- Cubic is a camera inferred from Keys cubic at scale 0.5 with a 24-term spectral product
  and 24-point Gauss-Legendre segment quadrature. It is not ordinary bicubic interpolation.
- Gaussian and ideal-lowpass cameras use the same camera-ratio construction, digital
  Nyquist bands, finite support, midpoint Fourier quadrature, window, and phase normalization.
- Reflect, symmetric, edge, wrap, and constant-zero boundaries handle singleton dimensions
  and supports wider than an image. Normalization precedes excluding constant-zero neighbors.
- The smaller scale runs first; ties preserve height before width. Parallel rows retain
  per-pixel accumulation order. Intermediate values are neither clipped nor linearized.
- Premultiplied-alpha handling belongs to the product wrapper. Final integer conversion
  clamps and rounds, then canonicalizes RGB to zero when quantized PNG alpha is zero.
- Alpha-channel presence is separate from opacity: RGB inputs resize three channels
  and produce RGB PNG; alpha-bearing inputs resize four premultiplied channels and
  produce RGBA PNG, including fully opaque alpha channels.

## Architecture and ownership

`src/core.rs` owns geometry, cameras, tables, padding, bounded buffers, and separable passes.
`src/codec.rs` plus `codec_limits.rs` own portable image I/O, color/orientation, HEIF/HEVC
preflights, and error translation. `src/main.rs` owns controls, the alpha wrapper, path safety,
and atomic publication. `src/bin/rro-numeric.rs` is a development interchange helper; the
normal image executable does not invoke it.

PNG/JPEG use Rust codecs. HEIF/HEVC use the selected OxideAV crates. ICC conversion is
explicit. Crop, rotation, and mirror transforms are supported; HEIF `iscl` scaling and
unsupported/HDR payloads fail before output publication. JPEG/HEIC composite onto white.
PNG/JPEG orientation is resolved before geometry: valid EXIF takes precedence over
XMP TIFF orientation, including explicit EXIF orientation 1. XMP attribute and element
forms use namespace-aware parsing with bounded metadata and no external entity resolution.

## Verification boundaries

Independent geometry/padding and transform tests, invalid-input/resource tests, and image
and codec interoperability checks validate the supported surface. Numeric agreement requires
maximum absolute error at most `1e-9` and RMSE at most `1e-10` before clipping. Requirements
remain in [SPEC.md](SPEC.md). Detailed audit outcomes and measurements are generated locally
under ignored `reports/`, separate from source and product archives.

The working-buffer budget is not a hard process-RSS cap. Repeated coded-node occurrences
are conservatively counted. Other operating systems, older macOS runtime compatibility,
the declared minimum compiler, signing, and notarization require their own qualification.
