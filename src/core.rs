//! Float64, separable camera-consistent spatial resizing.
//!
//! Numerical definitions follow the pinned MIT `ResizeRightOptics` reference.
//! Image decoding, alpha handling, and integer quantization belong to callers.
//! Resampling uses the caller's current Rayon pool and emits instability warnings
//! through the `log` facade; callers choose the logger and thread budget.
//!
//! # Examples
//!
//! ```
//! use rro::core::{resize, Image, ResizeOptions};
//!
//! # fn main() -> Result<(), rro::core::ResizeError> {
//! let image = Image { width: 3, height: 2, channels: 1, data: vec![0.5; 6] };
//! let options = ResizeOptions {
//!     scale_x: Some(0.5), scale_y: Some(0.5), ..ResizeOptions::default()
//! };
//! let smaller = resize(&image, &options)?;
//! assert_eq!((smaller.width, smaller.height), (2, 1));
//! # Ok(())
//! # }
//! ```
use rayon::prelude::*;
use std::collections::TryReserveError;
use std::error::Error;
use std::f64::consts::PI;
use std::fmt;

/// Bound the quadrature table independently of image dimensions.
const MAX_KERNEL_POINTS: usize = 1_048_576;
/// Bound total work even when a small image is paired with an enormous support.
const MAX_TAP_OPERATIONS: usize = 40_000_000_000;
/// Preserve the reference's headroom for signed coordinate and padding arithmetic.
const SAFE_INDEX: f64 = 2_305_843_009_213_693_952.0;

/// A validated resize failure, classified without exposing implementation details.
#[derive(Debug)]
pub struct ResizeError {
    message: String,
    kind: ResizeErrorKind,
}

#[derive(Debug)]
enum ResizeErrorKind {
    InvalidInput,
    ResourceLimit,
    Numerical,
    Allocation(TryReserveError),
}

impl ResizeError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: ResizeErrorKind::InvalidInput,
        }
    }

    fn resource(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: ResizeErrorKind::ResourceLimit,
        }
    }

    fn numerical(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: ResizeErrorKind::Numerical,
        }
    }

    fn allocation(what: &str, source: TryReserveError) -> Self {
        Self {
            message: format!("cannot allocate {what}"),
            kind: ResizeErrorKind::Allocation(source),
        }
    }

    /// Whether malformed image data, geometry, or controls caused this failure.
    pub fn is_invalid_input(&self) -> bool {
        matches!(self.kind, ResizeErrorKind::InvalidInput)
    }

    /// Whether work, address space, memory budget, or allocation capacity was exceeded.
    pub fn is_resource_limit(&self) -> bool {
        matches!(
            self.kind,
            ResizeErrorKind::ResourceLimit | ResizeErrorKind::Allocation(_)
        )
    }

    /// Whether a non-finite transfer, unstable phase sum, or output caused failure.
    pub fn is_numerical(&self) -> bool {
        matches!(self.kind, ResizeErrorKind::Numerical)
    }
}

impl fmt::Display for ResizeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ResizeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.kind {
            ResizeErrorKind::Allocation(source) => Some(source),
            _ => None,
        }
    }
}

/// An owned, row-major HWC buffer with real floating-point channel values.
///
/// Fields remain public because this is passive interchange data. [`resize`]
/// validates shape, buffer length, and finite values before processing it.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    /// Number of columns, excluding channels.
    pub width: usize,
    /// Number of rows.
    pub height: usize,
    /// Interleaved channels per pixel; the numeric core does not assign color meaning.
    pub channels: usize,
    /// Row-major pixels, with `width * height * channels` elements.
    pub data: Vec<f64>,
}

/// Camera spectrum from which a scale-dependent spatial kernel is constructed.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub enum Camera {
    /// Upstream Keys-cubic-derived camera at reference scale one half, with 24 terms.
    Cubic,
    /// Separable Gaussian point-spread function.
    Gaussian {
        /// Positive standard deviation in source-pixel units.
        sigma: f64,
    },
    /// Ideal band-limited reference camera, without a nonnegative physical PSF claim.
    IdealLowpass {
        /// Positive spectral cutoff in radians per source pixel.
        cutoff: f64,
    },
}

/// Boundary extension used when the finite kernel reaches beyond an image edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Padding {
    /// Reflect without repeating the boundary sample.
    Reflect,
    /// Reflect while repeating the boundary sample.
    Symmetric,
    /// Repeat the nearest boundary sample.
    Edge,
    /// Repeat the image periodically.
    Wrap,
    /// Use zero outside the image after normalizing each unmasked kernel phase.
    Constant,
}

/// Window applied only to the finite spatial support of a derived kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Window {
    /// Raised-cosine taper matching the upstream default.
    Hann,
    /// Sinc taper over the kernel support.
    Lanczos,
    /// No spatial taper.
    None,
}

/// Interpolation of the oversampled kernel table, not interpolation of image pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TableInterp {
    /// Four-point Keys cubic interpolation matching the upstream default.
    Cubic,
    /// Two-point linear interpolation.
    Linear,
}

/// Geometry and numerical controls for one separable image resize.
///
/// The default has upstream numerical settings but no geometry. Supply either
/// both scales, dimensions, or one dimension with aspect-ratio preservation.
/// Explicit scales may be combined with both dimensions; the requested scales
/// remain authoritative even when rounded dimension ratios differ.
#[derive(Clone, Debug, PartialEq)]
pub struct ResizeOptions {
    /// Intended column scale; must be supplied together with `scale_y`.
    pub scale_x: Option<f64>,
    /// Intended row scale; must be supplied together with `scale_x`.
    pub scale_y: Option<f64>,
    /// Explicit number of output columns.
    pub width: Option<usize>,
    /// Explicit number of output rows.
    pub height: Option<usize>,
    /// Camera shared by both separable axes.
    pub camera: Camera,
    /// Positive full spatial kernel width in source pixels, default 32.
    pub support: f64,
    /// Kernel lookup samples per source pixel, at least one, default eight.
    pub density: f64,
    /// Fourier quadrature period relative to support, at least two, default four.
    pub period_factor: f64,
    /// Nonnegative inverse regularization strength; zero preserves the default transfer.
    pub regularization: f64,
    /// Relative spectral cutoff threshold, nonnegative, default `1e-12`.
    pub rcond: f64,
    /// Extension rule for source coordinates outside the image.
    pub padding: Padding,
    /// Spatial truncation taper.
    pub window: Window,
    /// Interpolation rule for the derived lookup table.
    pub table_interp: TableInterp,
    /// Normalize each kernel phase before zero-padding contributions are excluded.
    pub normalize: bool,
    /// Estimated live working-buffer budget, default 2 GiB; excludes allocator overhead.
    pub max_memory_bytes: usize,
}

impl Default for ResizeOptions {
    fn default() -> Self {
        Self {
            scale_x: None,
            scale_y: None,
            width: None,
            height: None,
            camera: Camera::Cubic,
            support: 32.0,
            density: 8.0,
            period_factor: 4.0,
            regularization: 0.0,
            rcond: 1e-12,
            padding: Padding::Reflect,
            window: Window::Hann,
            table_interp: TableInterp::Cubic,
            normalize: true,
            max_memory_bytes: 2048 * 1024 * 1024,
        }
    }
}

// These conversions deliberately retain NumPy/Python binary64 coordinate
// semantics. Geometry and allocation limits are validated before indices are used.
#[expect(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "reference coordinate arithmetic converts pointer-sized integers to binary64"
)]
fn coordinate(value: usize) -> f64 {
    value as f64
}

#[expect(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "signed source offsets deliberately follow reference binary64 arithmetic"
)]
fn signed_coordinate(value: i64) -> f64 {
    value as f64
}

#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "callers validate nonnegative integral values below address and SAFE_INDEX limits"
)]
fn integral_length(value: f64) -> usize {
    debug_assert!(
        value.is_finite() && value >= 0.0 && value < coordinate(usize::MAX),
        "validated integral length out of range: {value}"
    );
    value as usize
}

#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "callers floor finite coordinates strictly inside signed SAFE_INDEX bounds"
)]
fn integral_index(value: f64) -> i64 {
    debug_assert!(
        value.is_finite() && value.abs() < SAFE_INDEX * 2.0,
        "validated source coordinate out of range: {value}"
    );
    value as i64
}

fn signed_index(value: usize) -> i64 {
    i64::try_from(value)
        .expect("validated image and kernel indices fit the signed coordinate range")
}

fn unsigned_index(value: i64) -> usize {
    usize::try_from(value)
        .expect("padding and table clamping produce nonnegative addressable indices")
}

fn positive(value: f64, name: &str) -> Result<(), ResizeError> {
    if value.is_finite() && value > 0.0 {
        Ok(())
    } else {
        Err(ResizeError::invalid(format!(
            "{name} must be finite and positive"
        )))
    }
}

fn checked_product(values: &[usize], what: &str) -> Result<usize, ResizeError> {
    values
        .iter()
        .try_fold(1usize, |a, &b| a.checked_mul(b))
        .ok_or_else(|| ResizeError::resource(format!("{what} size overflows addressable memory")))
}
fn checked_sum(values: &[usize], what: &str) -> Result<usize, ResizeError> {
    values
        .iter()
        .try_fold(0usize, |a, &b| a.checked_add(b))
        .ok_or_else(|| ResizeError::resource(format!("{what} size overflows addressable memory")))
}
fn zeroed<T: Default + Clone>(len: usize, what: &str) -> Result<Vec<T>, ResizeError> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(len)
        .map_err(|source| ResizeError::allocation(what, source))?;
    result.resize(len, T::default());
    Ok(result)
}

/// Match Python's `math.ulp` for finite, nonnegative IEEE binary64 numbers.
fn ulp(value: f64) -> f64 {
    if value == 0.0 || value < f64::MIN_POSITIVE {
        f64::from_bits(1)
    } else {
        let exponent = i32::try_from((value.to_bits() >> 52) & 0x7ff)
            .expect("masked exponent is at most 2047");
        let power = exponent - 1023 - 52;
        if power >= -1022 {
            f64::from_bits(
                u64::try_from(power + 1023).expect("normal exponent is nonnegative") << 52,
            )
        } else {
            f64::from_bits(1u64 << (power + 1074))
        }
    }
}

fn rounded_length(n: usize, scale: f64) -> Result<usize, ResizeError> {
    let length = coordinate(n) * scale;
    if !length.is_finite() || length >= coordinate(usize::MAX) || length >= SAFE_INDEX {
        return Err(ResizeError::resource(
            "scaled image dimension is not representable",
        ));
    }
    Ok(integral_length(
        (length - 8.0 * ulp(length)).ceil().max(1.0),
    ))
}

/// Resolve output dimensions and retain the exact requested per-axis scales.
///
/// Returns `(width, height, scale_x, scale_y)`. A lone width or height without
/// scales preserves aspect ratio; both explicit dimensions infer separate scales.
/// This checks geometry only; [`resize`] also validates the remaining controls.
///
/// # Errors
///
/// Returns an invalid-input error for incomplete, zero, non-finite, or
/// unrepresentable explicit geometry, or a resource-limit error for inferred
/// dimension overflow.
pub fn output_geometry(
    width: usize,
    height: usize,
    options: &ResizeOptions,
) -> Result<(usize, usize, f64, f64), ResizeError> {
    if width == 0
        || height == 0
        || coordinate(width) >= SAFE_INDEX
        || coordinate(height) >= SAFE_INDEX
    {
        return Err(ResizeError::invalid(
            "input dimensions must be positive and representable",
        ));
    }
    for dimension in [options.width, options.height].into_iter().flatten() {
        if dimension == 0 || coordinate(dimension) >= SAFE_INDEX {
            return Err(ResizeError::invalid(
                "output dimensions must be positive and representable",
            ));
        }
    }
    let (sx, sy, ow, oh) = match (
        options.scale_x,
        options.scale_y,
        options.width,
        options.height,
    ) {
        (Some(sx), Some(sy), None, None) => {
            positive(sx, "scale-x")?;
            positive(sy, "scale-y")?;
            (
                sx,
                sy,
                rounded_length(width, sx)?,
                rounded_length(height, sy)?,
            )
        }
        (Some(sx), Some(sy), Some(ow), Some(oh)) => (sx, sy, ow, oh),
        (None, None, Some(ow), Some(oh)) => (
            coordinate(ow) / coordinate(width),
            coordinate(oh) / coordinate(height),
            ow,
            oh,
        ),
        (None, None, Some(ow), None) => {
            let scale = coordinate(ow) / coordinate(width);
            (scale, scale, ow, rounded_length(height, scale)?)
        }
        (None, None, None, Some(oh)) => {
            let scale = coordinate(oh) / coordinate(height);
            (scale, scale, rounded_length(width, scale)?, oh)
        }
        (None, None, None, None) => {
            return Err(ResizeError::invalid("supply scale or output dimensions"));
        }
        _ => {
            return Err(ResizeError::invalid(
                "supply both axis scales; explicit scale with dimensions requires both width and height",
            ));
        }
    };
    positive(sx, "scale-x")?;
    positive(sy, "scale-y")?;
    for (n, m, s) in [(width, ow, sx), (height, oh, sy)] {
        let edge = coordinate(m - 1) / (2.0 * s) + coordinate(n - 1) / 2.0;
        if !edge.is_finite() || edge + options.support >= SAFE_INDEX {
            return Err(ResizeError::invalid(
                "requested scale projects coordinates outside the supported integer range",
            ));
        }
    }
    Ok((ow, oh, sx, sy))
}

fn validate_options(options: &ResizeOptions) -> Result<usize, ResizeError> {
    positive(options.support, "support")?;
    if !options.density.is_finite() || options.density < 1.0 {
        return Err(ResizeError::invalid(
            "density must be finite and at least 1",
        ));
    }
    if !options.period_factor.is_finite() || options.period_factor < 2.0 {
        return Err(ResizeError::invalid(
            "period-factor must be finite and at least 2",
        ));
    }
    if !options.regularization.is_finite()
        || options.regularization < 0.0
        || !options.rcond.is_finite()
        || options.rcond < 0.0
    {
        return Err(ResizeError::invalid(
            "regularization and rcond must be finite and nonnegative",
        ));
    }
    match options.camera {
        Camera::Gaussian { sigma } => positive(sigma, "gaussian sigma")?,
        Camera::IdealLowpass { cutoff } => positive(cutoff, "lowpass cutoff")?,
        Camera::Cubic => (),
    }
    if options.max_memory_bytes == 0 {
        return Err(ResizeError::invalid("memory budget must be positive"));
    }
    let points =
        2.0 * ((options.period_factor * options.support).max(16.0) * options.density / 2.0).ceil();
    if !points.is_finite() || points > coordinate(MAX_KERNEL_POINTS) {
        return Err(ResizeError::resource(format!(
            "kernel table exceeds {MAX_KERNEL_POINTS} samples; reduce support/density/period-factor"
        )));
    }
    // The Fourier grid cap also bounds the support and the number of taps.
    Ok(integral_length(points))
}

#[derive(Clone, Copy, Debug, Default)]
struct Complex {
    re: f64,
    im: f64,
}
impl Complex {
    fn mul(self, rhs: Self) -> Self {
        Self {
            re: self.re * rhs.re - self.im * rhs.im,
            im: self.re * rhs.im + self.im * rhs.re,
        }
    }
    fn phase(angle: f64) -> Self {
        let (im, re) = angle.sin_cos();
        Self { re, im }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TransformDirection {
    Forward,
    Inverse,
}

/// Iterative in-place DFT for power-of-two lengths, positive sign for inverse.
fn radix2(data: &mut [Complex], direction: TransformDirection) {
    let inverse = direction == TransformDirection::Inverse;
    let n = data.len();
    debug_assert!(
        n.is_power_of_two(),
        "radix-2 transform requires a power-of-two length, got {n}"
    );
    let mut reverse = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while reverse & bit != 0 {
            reverse ^= bit;
            bit >>= 1;
        }
        reverse ^= bit;
        if i < reverse {
            data.swap(i, reverse);
        }
    }
    let mut width = 2;
    while width <= n {
        let step = Complex::phase(if inverse {
            2.0 * PI / coordinate(width)
        } else {
            -2.0 * PI / coordinate(width)
        });
        for block in data.chunks_exact_mut(width) {
            let mut phase = Complex { re: 1.0, im: 0.0 };
            for j in 0..width / 2 {
                let a = block[j];
                let b = block[j + width / 2].mul(phase);
                block[j] = Complex {
                    re: a.re + b.re,
                    im: a.im + b.im,
                };
                block[j + width / 2] = Complex {
                    re: a.re - b.re,
                    im: a.im - b.im,
                };
                phase = phase.mul(step);
            }
        }
        width *= 2;
    }
    if inverse {
        for x in data {
            x.re /= coordinate(n);
            x.im /= coordinate(n);
        }
    }
}

fn chirp_phase(index: usize, length: usize) -> f64 {
    let index = u64::try_from(index).expect("FFT length is bounded by MAX_KERNEL_POINTS");
    let modulus = u64::try_from(2 * length).expect("twice the bounded FFT length fits u64");
    let reduced = u32::try_from((index * index) % modulus)
        .expect("phase modulo twice the bounded FFT length fits u32");
    PI * f64::from(reduced) / coordinate(length)
}

fn inverse_fft(data: &mut [Complex]) -> Result<(), ResizeError> {
    let n = data.len();
    if n.is_power_of_two() {
        radix2(data, TransformDirection::Inverse);
        return Ok(());
    }
    // Bluestein converts an arbitrary-length inverse transform into a
    // power-of-two convolution. It retains the reference quadrature length.
    let m = (2 * n - 1).next_power_of_two();
    let mut a: Vec<Complex> = zeroed(m, "FFT convolution")?;
    let mut b: Vec<Complex> = zeroed(m, "FFT chirp")?;
    for i in 0..n {
        // Reduce the squared integer before conversion to bound phase error.
        let phase = chirp_phase(i, n);
        a[i] = data[i].mul(Complex::phase(phase));
        let chirp = Complex::phase(-phase);
        b[i] = chirp;
        if i != 0 {
            b[m - i] = chirp;
        }
    }
    radix2(&mut a, TransformDirection::Forward);
    radix2(&mut b, TransformDirection::Forward);
    for (x, y) in a.iter_mut().zip(b) {
        *x = x.mul(y);
    }
    radix2(&mut a, TransformDirection::Inverse);
    for i in 0..n {
        let phase = chirp_phase(i, n);
        let value = a[i].mul(Complex::phase(phase));
        data[i] = Complex {
            re: value.re / coordinate(n),
            im: value.im / coordinate(n),
        };
    }
    Ok(())
}

fn keys(x: f64) -> f64 {
    let x = x.abs();
    if x <= 1.0 {
        1.5 * x.powi(3) - 2.5 * x.powi(2) + 1.0
    } else if x < 2.0 {
        -0.5 * x.powi(3) + 2.5 * x.powi(2) - 4.0 * x + 2.0
    } else {
        0.0
    }
}

fn camera_spectrum(camera: Camera, omega: f64) -> f64 {
    match camera {
        Camera::Gaussian { sigma } => (-0.5 * (omega * sigma).powi(2)).exp(),
        Camera::IdealLowpass { cutoff } => {
            if omega.abs() < cutoff {
                1.0
            } else {
                0.0
            }
        }
        Camera::Cubic => {
            let mut result = 1.0;
            for j in 0..24 {
                let w = omega * 0.5_f64.powi(j + 1);
                let value: f64 = CUBIC_QUADRATURE
                    .iter()
                    .map(|&(node, weight)| (w * node / 0.5).cos() * weight)
                    .sum();
                result *= value;
            }
            result
        }
    }
}

struct Kernel {
    table: Vec<f64>,
    support: f64,
    density: f64,
    window: Window,
    interpolation: TableInterp,
}
impl Kernel {
    fn new(scale: f64, options: &ResizeOptions, points: usize) -> Result<Self, ResizeError> {
        let mut values: Vec<Complex> = zeroed(points, "kernel Fourier table")?;
        let camera_dc = camera_spectrum(options.camera, 0.0).abs();
        let mut high_gain = false;
        for (i, value) in values.iter_mut().enumerate() {
            let k = if i < points / 2 {
                coordinate(i)
            } else {
                coordinate(i) - coordinate(points)
            };
            // Keep fftfreq's operation order: 1/(N*(1/density)).
            let omega = 2.0
                * PI
                * (k * (1.0 / (coordinate(points) * (1.0 / options.density)))
                    + options.density / (2.0 * coordinate(points)));
            let scaled = omega / scale;
            if omega.abs() < PI && scaled.abs() < PI {
                let source = camera_spectrum(options.camera, omega);
                let target = camera_spectrum(options.camera, scaled);
                if source.abs() > options.rcond * camera_dc
                    && target.abs() > options.rcond * camera_dc
                {
                    value.re =
                        target * source / (source.abs().powi(2) + options.regularization.powi(2));
                    if !value.re.is_finite() {
                        return Err(ResizeError::numerical(
                            "camera transfer is non-finite; choose regularization or different camera parameters",
                        ));
                    }
                    high_gain |= value.re.abs() > 1000.0;
                }
            }
        }
        if high_gain {
            log::warn!(
                "camera inverse gain exceeds 1000; finite kernels and noise may be unstable; consider --regularization"
            );
        }
        inverse_fft(&mut values)?;
        let mut table = zeroed(points, "kernel spatial table")?;
        for (i, value) in table.iter_mut().enumerate() {
            let shifted = values[(i + points / 2) % points];
            let phase =
                Complex::phase(PI * (coordinate(i) - coordinate(points / 2)) / coordinate(points));
            *value = shifted.mul(phase).re * options.density;
        }
        Ok(Self {
            table,
            support: options.support,
            density: options.density,
            window: options.window,
            interpolation: options.table_interp,
        })
    }
    fn at(&self, offset: f64) -> f64 {
        if offset.abs() > self.support / 2.0 {
            return 0.0;
        }
        let table_coordinate = offset * self.density + coordinate(self.table.len() / 2);
        let base = integral_index(table_coordinate.floor());
        let fraction = table_coordinate - table_coordinate.floor();
        let range = if self.interpolation == TableInterp::Cubic {
            -1..=2
        } else {
            0..=1
        };
        let mut value = 0.0;
        for bit in range {
            let index = unsigned_index((base + bit).clamp(0, signed_index(self.table.len()) - 1));
            let weight = if self.interpolation == TableInterp::Cubic {
                keys(fraction - signed_coordinate(bit))
            } else if bit == 0 {
                1.0 - fraction
            } else {
                fraction
            };
            value += self.table[index] * weight;
        }
        let u = 2.0 * offset / self.support;
        value
            * match self.window {
                Window::Hann => f64::midpoint(1.0, (PI * u).cos()),
                Window::Lanczos => {
                    if u == 0.0 {
                        1.0
                    } else {
                        (PI * u).sin() / (PI * u)
                    }
                }
                Window::None => 1.0,
            }
    }
}

#[derive(Clone, Copy, Default)]
struct Neighbor {
    index: usize,
    weight: f64,
}
fn padded_index(index: i64, size: usize, padding: Padding) -> (usize, bool) {
    let n = signed_index(size);
    let valid = index >= 0 && index < n;
    let mapped = match padding {
        Padding::Constant | Padding::Edge => index.clamp(0, n - 1),
        Padding::Wrap => index.rem_euclid(n),
        Padding::Reflect => {
            if n == 1 {
                0
            } else {
                let t = index.rem_euclid(2 * (n - 1));
                t.min(2 * (n - 1) - t)
            }
        }
        Padding::Symmetric => {
            let t = index.rem_euclid(2 * n);
            t.min(2 * n - 1 - t)
        }
    };
    (
        unsigned_index(mapped),
        padding != Padding::Constant || valid,
    )
}

fn projected_grid(input: usize, output: usize, scale: f64, j: usize) -> f64 {
    (coordinate(j) - coordinate(output - 1) / 2.0) / scale + coordinate(input - 1) / 2.0
}

fn make_map(
    input: usize,
    output: usize,
    scale: f64,
    kernel: &Kernel,
    options: &ResizeOptions,
) -> Result<Vec<Neighbor>, ResizeError> {
    let taps = integral_length(options.support.ceil()) + 2;
    let mut map: Vec<Neighbor> = zeroed(
        checked_product(&[output, taps], "neighbor map")?,
        "neighbor map",
    )?;
    for (j, row) in map.chunks_exact_mut(taps).enumerate() {
        let center = projected_grid(input, output, scale, j);
        let left = integral_index((center - options.support / 2.0).floor());
        let mut sum = 0.0;
        for (tap, neighbor) in row.iter_mut().enumerate() {
            let index = left + signed_index(tap);
            neighbor.weight = kernel.at(center - signed_coordinate(index));
            sum += neighbor.weight;
        }
        if options.normalize && sum.abs() < 32.0 * f64::EPSILON {
            return Err(ResizeError::numerical(
                "kernel has a nearly zero phase sum; enlarge support or disable normalize",
            ));
        }
        for (tap, neighbor) in row.iter_mut().enumerate() {
            if options.normalize {
                neighbor.weight /= sum;
            }
            let (index, valid) = padded_index(left + signed_index(tap), input, options.padding);
            neighbor.index = index;
            if !valid {
                neighbor.weight = 0.0;
            }
            if !neighbor.weight.is_finite() {
                return Err(ResizeError::numerical("kernel weights are non-finite"));
            }
        }
    }
    Ok(map)
}

#[derive(Clone, Copy, Debug)]
enum Axis {
    Horizontal,
    Vertical,
}

fn pass(
    input: &Image,
    axis: Axis,
    output_size: usize,
    map: &[Neighbor],
    taps: usize,
) -> Result<Image, ResizeError> {
    let vertical = matches!(axis, Axis::Vertical);
    let (width, height) = if vertical {
        (input.width, output_size)
    } else {
        (output_size, input.height)
    };
    let count = checked_product(&[width, height, input.channels], "output image")?;
    let mut data: Vec<f64> = zeroed(count, "resized image")?;
    let row_len = width * input.channels;
    if vertical {
        data.par_chunks_exact_mut(row_len)
            .enumerate()
            .for_each(|(y, destination)| {
                for neighbor in &map[y * taps..(y + 1) * taps] {
                    let offset = neighbor.index * row_len;
                    let source = &input.data[offset..offset + row_len];
                    for (out, &value) in destination.iter_mut().zip(source) {
                        *out += value * neighbor.weight;
                    }
                }
            });
    } else {
        let channels = input.channels;
        data.par_chunks_exact_mut(row_len)
            .enumerate()
            .for_each(|(y, destination)| {
                let source =
                    &input.data[y * input.width * channels..(y + 1) * input.width * channels];
                for (x, pixel) in destination.chunks_exact_mut(channels).enumerate() {
                    let neighbors = &map[x * taps..(x + 1) * taps];
                    // Common image channel counts avoid a bounds-checked inner
                    // channel loop while retaining summation order per channel.
                    if channels == 4 {
                        let mut a = [0.0; 4];
                        for neighbor in neighbors {
                            let s = &source[neighbor.index * 4..neighbor.index * 4 + 4];
                            a[0] += s[0] * neighbor.weight;
                            a[1] += s[1] * neighbor.weight;
                            a[2] += s[2] * neighbor.weight;
                            a[3] += s[3] * neighbor.weight;
                        }
                        pixel.copy_from_slice(&a);
                    } else if channels == 3 {
                        let mut a = [0.0; 3];
                        for neighbor in neighbors {
                            let s = &source[neighbor.index * 3..neighbor.index * 3 + 3];
                            a[0] += s[0] * neighbor.weight;
                            a[1] += s[1] * neighbor.weight;
                            a[2] += s[2] * neighbor.weight;
                        }
                        pixel.copy_from_slice(&a);
                    } else {
                        for neighbor in neighbors {
                            let s =
                                &source[neighbor.index * channels..(neighbor.index + 1) * channels];
                            for (out, &value) in pixel.iter_mut().zip(s) {
                                *out += value * neighbor.weight;
                            }
                        }
                    }
                }
            });
    }
    if data.par_iter().any(|value| !value.is_finite()) {
        return Err(ResizeError::numerical(
            "resampling produced non-finite pixels; use regularization or different camera parameters",
        ));
    }
    Ok(Image {
        width,
        height,
        channels: input.channels,
        data,
    })
}

#[expect(
    clippy::float_cmp,
    reason = "kernel reuse requires identical requested scales, never an approximate snap"
)]
fn identical_scale(left: f64, right: f64) -> bool {
    left == right
}

/// Resize an interleaved float64 image without clipping intermediate channel values.
///
/// The smaller scale runs first; ties run rows before columns. The caller's
/// current Rayon pool provides parallelism, and inverse gains over 1000 emit
/// a warning through `log`. Image colors and alpha are the caller's responsibility.
///
/// # Errors
///
/// Returns [`ResizeError`] for malformed or non-finite image data, invalid
/// controls/geometry, work or allocation limits, failed allocation, a nearly zero
/// normalized phase sum, or a non-finite camera transfer/output.
pub fn resize(input: &Image, options: &ResizeOptions) -> Result<Image, ResizeError> {
    let points = validate_options(options)?;
    if input.channels == 0 {
        return Err(ResizeError::invalid("image channels must be positive"));
    }
    let source_elements =
        checked_product(&[input.width, input.height, input.channels], "input image")?;
    if input.data.len() != source_elements {
        return Err(ResizeError::invalid(
            "image dimensions do not match its pixel buffer",
        ));
    }
    if input.data.iter().any(|value| !value.is_finite()) {
        return Err(ResizeError::invalid("input contains non-finite pixels"));
    }
    let (width, height, sx, sy) = output_geometry(input.width, input.height, options)?;
    let y_first = sy <= sx;
    let middle_elements = checked_product(
        &[
            if y_first { input.width } else { width },
            if y_first { height } else { input.height },
            input.channels,
        ],
        "intermediate image",
    )?;
    let final_elements = checked_product(&[width, height, input.channels], "output image")?;
    let taps = integral_length(options.support.ceil()) + 2;
    let operations = checked_product(
        &[
            checked_sum(&[middle_elements, final_elements], "resampling work")?,
            taps,
        ],
        "resampling work",
    )?;
    if operations > MAX_TAP_OPERATIONS {
        return Err(ResizeError::resource(format!(
            "resampling exceeds {MAX_TAP_OPERATIONS} tap operations; reduce image size or support"
        )));
    }
    let buffers = checked_product(
        &[
            checked_sum(
                &[source_elements, middle_elements, final_elements],
                "live image buffers",
            )?,
            8,
        ],
        "live image buffers",
    )?;
    let maps = checked_product(
        &[
            checked_sum(&[width, height], "axis dimensions")?,
            taps,
            std::mem::size_of::<Neighbor>(),
        ],
        "neighbor maps",
    )?;
    // 256 bytes per quadrature point exceeds both radix-2 and Bluestein's
    // simultaneous complex buffers plus two retained real kernel tables.
    let kernels = checked_product(&[points, 256], "kernel workspace")?;
    let budget = checked_sum(&[buffers, maps, kernels], "working buffers")?;
    if budget > options.max_memory_bytes {
        return Err(ResizeError::resource(format!(
            "estimated working buffers require {budget} bytes; memory budget is {} bytes",
            options.max_memory_bytes
        )));
    }
    let kernel_x = Kernel::new(sx, options, points)?;
    let other_kernel;
    let kernel_y = if identical_scale(sx, sy) {
        &kernel_x
    } else {
        other_kernel = Kernel::new(sy, options, points)?;
        &other_kernel
    };
    let x_map = make_map(input.width, width, sx, &kernel_x, options)?;
    let y_map = make_map(input.height, height, sy, kernel_y, options)?;
    if y_first {
        let middle = pass(input, Axis::Vertical, height, &y_map, taps)?;
        pass(&middle, Axis::Horizontal, width, &x_map, taps)
    } else {
        let middle = pass(input, Axis::Horizontal, width, &x_map, taps)?;
        pass(&middle, Axis::Vertical, height, &y_map, taps)
    }
}

// 24-point Gauss-Legendre nodes mapped separately to [0,1] and [1,2].
// Weights include the even-symmetry factor and Keys cubic (a=-1/2).
// Constants generated from the pinned NumPy reference, not learned data.
// Retain the oracle's round-trippable decimal coefficients verbatim.
#[expect(
    clippy::excessive_precision,
    clippy::unreadable_literal,
    reason = "generated quadrature literals retain the oracle binary64 coefficients verbatim"
)]
const CUBIC_QUADRATURE: [(f64, f64); 48] = [
    (2.40639000148934468e-03, 1.23410513966493978e-02),
    (1.26357220143452631e-02, 2.85200865607106099e-02),
    (3.08627239986336011e-02, 4.41739547449758324e-02),
    (5.67922364977994643e-02, 5.88367300378368485e-02),
    (8.99990070130485265e-02, 7.19414496594919378e-02),
    (1.29937904210722821e-01, 8.28357380549596400e-02),
    (1.75953174031512227e-01, 9.08607412408945075e-02),
    (2.27289264305580219e-01, 9.54601253924860088e-02),
    (2.83103246186977464e-01, 9.62931926236606922e-02),
    (3.42478660151918302e-01, 9.33243845520592558e-02),
    (4.04440566263191859e-01, 8.68658985647569465e-02),
    (4.67971553568697185e-01, 7.75604039948685070e-02),
    (5.32028446431302759e-01, 6.63044449021238241e-02),
    (5.95559433736808197e-01, 5.41266908289794180e-02),
    (6.57521339848081698e-01, 4.20453944418515135e-02),
    (7.16896753813022536e-01, 3.09337479574543599e-02),
    (7.72710735694419837e-01, 2.14193017723678970e-02),
    (8.24046825968487773e-01, 1.38349534250818626e-02),
    (8.70062095789277179e-01, 8.22649668839593555e-03),
    (9.10000992986951474e-01, 4.40854030417535316e-03),
    (9.43207763502200480e-01, 2.05007492047967364e-03),
    (9.69137276001366343e-01, 7.65657957810800065e-04),
    (9.87364277985654737e-01, 1.89281734185734498e-04),
    (9.97593609998510655e-01, 1.49915770768481392e-05),
    (1.00240639000148946e+00, -1.47775274662812199e-05),
    (1.01263572201434515e+00, -1.75730764379334838e-04),
    (1.03086272399863366e+00, -6.41737395504000832e-04),
    (1.05679223649779952e+00, -1.49802147459424041e-03),
    (1.08999900701304853e+00, -2.73319576529897253e-03),
    (1.12993790421072271e+00, -4.23900622915132468e-03),
    (1.17595317403151212e+00, -5.83181446034651465e-03),
    (1.22728926430558016e+00, -7.29064714463884250e-03),
    (1.28310324618697735e+00, -8.40294222544315898e-03),
    (1.34247866015191830e+00, -9.00758607957588726e-03),
    (1.40444056626319180e+00, -9.02578245562245772e-03),
    (1.46797155356869724e+00, -8.47343274518373712e-03),
    (1.53202844643130276e+00, -7.45322080505617796e-03),
    (1.59555943373680820e+00, -6.12935059128552531e-03),
    (1.65752133984808170e+00, -4.69171998653148802e-03),
    (1.71689675381302265e+00, -3.31833030194606578e-03),
    (1.77271073569441984e+00, -2.14450990424938691e-03),
    (1.82404682596848788e+00, -1.24522810151574473e-03),
    (1.87006209578927729e+00, -6.33066982250798872e-04),
    (1.91000099298695147e+00, -2.70312787288039486e-04),
    (1.94320776350220048e+00, -9.01985682857515925e-05),
    (1.96913727600136634e+00, -2.04364898631691145e-05),
    (1.98736427798565485e+00, -2.24890158331385503e-06),
    (1.99759360999851054e+00, -3.56462731677621260e-08),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_errors_preserve_classification_and_allocation_cause() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ResizeError>();
        assert_send_sync::<Image>();
        assert_send_sync::<ResizeOptions>();

        let allocation = zeroed::<u8>(usize::MAX, "test buffer")
            .expect_err("usize::MAX exceeds Vec addressable allocation capacity");
        assert!(
            allocation.is_resource_limit(),
            "allocation failure category"
        );
        assert!(
            allocation.source().is_some(),
            "allocation cause must remain inspectable"
        );
        assert!(
            allocation.to_string().contains("test buffer"),
            "allocation context"
        );

        let input = Image {
            width: 1,
            height: 1,
            channels: 1,
            data: vec![0.5],
        };
        let invalid =
            resize(&input, &ResizeOptions::default()).expect_err("missing geometry must fail");
        assert!(invalid.is_invalid_input(), "invalid geometry category");

        let numerical = resize(
            &input,
            &ResizeOptions {
                scale_x: Some(0.5),
                scale_y: Some(0.5),
                rcond: 1.0,
                ..ResizeOptions::default()
            },
        )
        .expect_err("discarding every frequency has a zero normalized phase sum");
        assert!(numerical.is_numerical(), "zero phase sum category");
    }

    #[test]
    fn inverse_transform_matches_direct_sum_even_and_prime_factors() {
        for n in [2, 6, 8, 10, 14, 18, 32, 66, 130] {
            let input: Vec<Complex> = (0..n)
                .map(|i| Complex {
                    re: coordinate(i * 7 + 3).sin(),
                    im: coordinate(i * 11).cos(),
                })
                .collect();
            let mut actual = input.clone();
            inverse_fft(&mut actual).unwrap();
            for (j, got) in actual.iter().enumerate() {
                let mut expected = Complex::default();
                for (k, value) in input.iter().enumerate() {
                    let v = value.mul(Complex::phase(2.0 * PI * coordinate(j * k) / coordinate(n)));
                    expected.re += v.re / coordinate(n);
                    expected.im += v.im / coordinate(n);
                }
                assert!((got.re - expected.re).abs() < 2e-13, "n={n} j={j}");
                assert!((got.im - expected.im).abs() < 2e-13, "n={n} j={j}");
            }
        }
    }

    #[test]
    fn centered_grid_is_symmetric_and_keeps_requested_scale() {
        for (n, m, s) in [
            (1, 5, 3.0),
            (7, 4, 0.5),
            (10, 7, 0.66),
            (17, 25, 2f64.sqrt()),
        ] {
            let first = projected_grid(n, m, s, 0);
            let last = projected_grid(n, m, s, m - 1);
            assert!(
                (f64::midpoint(first, last) - coordinate(n - 1) / 2.0).abs() < 1e-14,
                "grid center for {n}->{m} at scale {s}"
            );
            assert!(
                (projected_grid(n, m, s, 1) - first - 1.0 / s).abs() < 2e-14,
                "grid spacing for {n}->{m} at scale {s}"
            );
        }
        let o = ResizeOptions {
            scale_x: Some(0.5),
            scale_y: Some(0.5),
            width: Some(7),
            height: Some(5),
            ..Default::default()
        };
        let (width, height, scale_x, scale_y) = output_geometry(11, 9, &o).unwrap();
        assert_eq!((width, height), (7, 5));
        assert_eq!(
            (scale_x.to_bits(), scale_y.to_bits()),
            (0.5_f64.to_bits(), 0.5_f64.to_bits())
        );
    }

    #[test]
    fn ceil_guard_matches_upstream_integer_boundary() {
        assert_eq!(rounded_length(10, 0.1 + f64::EPSILON / 8.0).unwrap(), 1);
        assert_eq!(rounded_length(10, 0.100_000_000_01).unwrap(), 2);
        assert_eq!(rounded_length(3, 0.01).unwrap(), 1);
        assert_eq!(ulp(0.0).to_bits(), 1);
        assert_eq!(ulp(1.0).to_bits(), f64::EPSILON.to_bits());
        assert_eq!(ulp(0.5).to_bits(), (f64::EPSILON / 2.0).to_bits());
    }

    #[test]
    fn padding_matches_explicit_expected_extensions() {
        let tests = [
            (
                Padding::Reflect,
                vec![0, 1, 2, 1, 0, 1, 2, 1, 0, 1, 2, 1, 0],
            ),
            (
                Padding::Symmetric,
                vec![2, 2, 1, 0, 0, 1, 2, 2, 1, 0, 0, 1, 2],
            ),
            (Padding::Wrap, vec![2, 0, 1, 2, 0, 1, 2, 0, 1, 2, 0, 1, 2]),
            (Padding::Edge, vec![0, 0, 0, 0, 0, 1, 2, 2, 2, 2, 2, 2, 2]),
        ];
        for (mode, expected) in tests {
            let actual: Vec<usize> = (-4..9).map(|i| padded_index(i, 3, mode).0).collect();
            assert_eq!(actual, expected, "{mode:?}");
        }
        for mode in [
            Padding::Reflect,
            Padding::Symmetric,
            Padding::Wrap,
            Padding::Edge,
            Padding::Constant,
        ] {
            for i in -40..40 {
                assert_eq!(
                    padded_index(i, 1, mode).0,
                    0,
                    "singleton padding {mode:?} at {i}"
                );
            }
        }
        assert_eq!(padded_index(-1, 3, Padding::Constant), (0, false));
        assert_eq!(padded_index(2, 3, Padding::Constant), (2, true));
    }

    #[test]
    fn unit_scale_is_identity_or_centered_crop() {
        let image = Image {
            width: 9,
            height: 7,
            channels: 3,
            data: (0..9 * 7 * 3).map(|i| coordinate(i) / 100.0).collect(),
        };
        let o = ResizeOptions {
            scale_x: Some(1.0),
            scale_y: Some(1.0),
            ..Default::default()
        };
        let actual = resize(&image, &o).unwrap();
        assert!(
            actual
                .data
                .iter()
                .zip(&image.data)
                .all(|(a, b)| (a - b).abs() < 1e-12)
        );
        let cropped = resize(
            &image,
            &ResizeOptions {
                width: Some(5),
                height: Some(3),
                ..o
            },
        )
        .unwrap();
        for y in 0..3 {
            for x in 0..5 {
                for c in 0..3 {
                    assert!(
                        (cropped.data[(y * 5 + x) * 3 + c]
                            - image.data[((y + 2) * 9 + x + 2) * 3 + c])
                            .abs()
                            < 1e-12
                    );
                }
            }
        }
    }

    #[test]
    fn constant_image_and_singleton_axes_are_preserved() {
        for (width, height) in [(1, 1), (1, 7), (9, 1), (11, 13)] {
            let image = Image {
                width,
                height,
                channels: 4,
                data: vec![0.375; width * height * 4],
            };
            for camera in [
                Camera::Cubic,
                Camera::Gaussian { sigma: 0.7 },
                Camera::IdealLowpass { cutoff: PI },
            ] {
                let o = ResizeOptions {
                    scale_x: Some(0.6),
                    scale_y: Some(1.5),
                    camera,
                    ..Default::default()
                };
                assert!(
                    resize(&image, &o)
                        .unwrap()
                        .data
                        .iter()
                        .all(|v| (v - 0.375).abs() < 1e-12)
                );
            }
        }
    }

    #[test]
    fn invalid_options_and_allocation_are_rejected() {
        let image = Image {
            width: 2,
            height: 2,
            channels: 1,
            data: vec![1.0; 4],
        };
        let base = ResizeOptions {
            scale_x: Some(0.5),
            scale_y: Some(0.5),
            ..Default::default()
        };
        for o in [
            ResizeOptions {
                scale_x: Some(f64::NAN),
                ..base
            },
            ResizeOptions {
                support: f64::INFINITY,
                ..base
            },
            ResizeOptions {
                density: 0.0,
                ..base
            },
            ResizeOptions {
                regularization: -1.0,
                ..base
            },
            ResizeOptions {
                rcond: f64::NAN,
                ..base
            },
            ResizeOptions {
                camera: Camera::Gaussian { sigma: 0.0 },
                ..base
            },
            ResizeOptions {
                max_memory_bytes: 1,
                ..base
            },
            ResizeOptions {
                support: 1e20,
                ..base
            },
            ResizeOptions {
                scale_x: Some(1e-300),
                width: Some(2),
                height: Some(2),
                ..base
            },
            ResizeOptions {
                width: Some(usize::MAX),
                height: Some(usize::MAX),
                ..base
            },
        ] {
            assert!(resize(&image, &o).is_err(), "accepted {o:?}");
        }
        let bad = Image {
            width: usize::MAX,
            height: usize::MAX,
            channels: 4,
            data: vec![],
        };
        assert!(resize(&bad, &base).is_err());
        let bad = Image {
            width: 2,
            height: 2,
            channels: 1,
            data: vec![f64::NAN; 4],
        };
        assert!(resize(&bad, &base).is_err());
    }
}
