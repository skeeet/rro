//! Standalone image conversion with safe publication and explicit pixel policy.
mod bin_support;

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, ValueEnum};
use rro::codec::{self, AlphaChannel, OutputFormat, RgbaImage};
use rro::core::{self, Camera, Image, Padding, ResizeOptions, TableInterp, Window};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::str::FromStr;
use std::time::Instant;

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CameraArg {
    Cubic,
    Gaussian,
    IdealLowpass,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
enum PaddingArg {
    Reflect,
    Symmetric,
    Edge,
    Wrap,
    Constant,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
enum WindowArg {
    Hann,
    Lanczos,
    None,
}
#[derive(Clone, Copy, Debug, ValueEnum)]
enum InterpArg {
    Cubic,
    Linear,
}

/// Resize PNG, JPEG, or HEIC images using camera-derived spatial kernels.
#[derive(Debug, Parser)]
#[command(
    version,
    long_about = "Standalone camera-consistent image resizing for macOS.\nInputs and outputs: PNG, JPEG, HEIC/HEIF. Images become SDR 8-bit sRGB.\nPNG preserves source alpha-channel presence. JPEG/HEIC composite transparency onto white.\nEXIF orientation is applied; other source metadata is not copied."
)]
struct Args {
    /// Source image (never modified)
    input: PathBuf,
    /// Destination image; extension selects encoding
    output: PathBuf,
    /// Shared scale; preserves intended scale even with explicit dimensions
    #[arg(long, conflicts_with_all = ["scale_x", "scale_y"])]
    scale: Option<f64>,
    #[arg(long, requires = "scale_y")]
    scale_x: Option<f64>,
    #[arg(long, requires = "scale_x")]
    scale_y: Option<f64>,
    /// Output columns; width alone preserves aspect ratio
    #[arg(long)]
    width: Option<usize>,
    /// Output rows; height alone preserves aspect ratio
    #[arg(long)]
    height: Option<usize>,
    #[arg(long, value_enum, default_value = "cubic")]
    camera: CameraArg,
    /// Gaussian PSF sigma in source pixels (default 0.7)
    #[arg(long)]
    sigma: Option<f64>,
    /// Ideal-lowpass cutoff, radians per pixel (default pi)
    #[arg(long)]
    cutoff: Option<f64>,
    #[arg(long, default_value_t = 0.0)]
    regularization: f64,
    #[arg(long, default_value_t = 1e-12)]
    rcond: f64,
    #[arg(long, default_value_t = 32.0)]
    support: f64,
    #[arg(long, default_value_t = 8.0)]
    density: f64,
    #[arg(long, default_value_t = 4.0)]
    period_factor: f64,
    #[arg(long, value_enum, default_value = "reflect")]
    padding: PaddingArg,
    #[arg(long, value_enum, default_value = "hann")]
    window: WindowArg,
    #[arg(long, value_enum, default_value = "cubic")]
    table_interp: InterpArg,
    #[arg(long)]
    no_normalize: bool,
    /// JPEG/HEIC encoding quality, 0 through 1
    #[arg(long, default_value_t = 0.9)]
    quality: f64,
    /// Worker count; omitted uses available CPU parallelism
    #[arg(long)]
    threads: Option<usize>,
    /// Working-buffer budget; system codec/allocator overhead is additional
    #[arg(long, default_value_t = 2048)]
    max_memory_mib: usize,
    /// Replace an existing destination atomically
    #[arg(long)]
    force: bool,
    /// Print decode, resize, encode, and total durations to stderr
    #[arg(long)]
    timings: bool,
}

fn format_for(path: &Path) -> Result<OutputFormat> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    OutputFormat::from_str(&extension).with_context(|| {
        format!(
            "unsupported image extension for {}; use .png, .jpg, .jpeg, .heic, or .heif",
            path.display()
        )
    })
}

fn reject_alias(input: &Path, output: &Path) -> Result<()> {
    let source =
        fs::metadata(input).with_context(|| format!("cannot inspect input {}", input.display()))?;
    ensure!(source.is_file(), "input must be a regular file");
    match same_file::is_same_file(input, output) {
        Ok(true) => bail!("input and output identify the same file; choose another output"),
        Ok(false) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "cannot compare input {} and output {}",
                input.display(),
                output.display()
            )
        }),
    }
}

impl Args {
    fn resize_options(&self) -> Result<ResizeOptions> {
        ensure!(
            self.quality.is_finite() && (0.0..=1.0).contains(&self.quality),
            "quality must be finite and between 0 and 1"
        );
        ensure!(
            !self.threads.is_some_and(|count| count == 0 || count > 256),
            "threads must be between 1 and 256"
        );
        ensure!(self.max_memory_mib > 0, "max-memory-mib must be positive");
        let budget = self
            .max_memory_mib
            .checked_mul(1024 * 1024)
            .context("memory budget overflows")?;
        let scale = self.scale.or(self.scale_x);
        ensure!(
            scale.is_none() || self.width.is_some() == self.height.is_some(),
            "explicit scale requires either both width and height, or neither"
        );
        ensure!(
            scale.is_some() || self.width.is_some() || self.height.is_some(),
            "supply --scale, --scale-x/--scale-y, --width, or --height"
        );
        let camera = match self.camera {
            CameraArg::Cubic => {
                ensure!(
                    self.sigma.is_none() && self.cutoff.is_none(),
                    "sigma/cutoff do not apply to cubic camera"
                );
                Camera::Cubic
            }
            CameraArg::Gaussian => {
                ensure!(
                    self.cutoff.is_none(),
                    "cutoff applies only to ideal-lowpass camera"
                );
                Camera::Gaussian {
                    sigma: self.sigma.unwrap_or(0.7),
                }
            }
            CameraArg::IdealLowpass => {
                ensure!(
                    self.sigma.is_none(),
                    "sigma applies only to gaussian camera"
                );
                Camera::IdealLowpass {
                    cutoff: self.cutoff.unwrap_or(std::f64::consts::PI),
                }
            }
        };
        let options = ResizeOptions {
            scale_x: self.scale.or(self.scale_x),
            scale_y: self.scale.or(self.scale_y),
            width: self.width,
            height: self.height,
            camera,
            support: self.support,
            density: self.density,
            period_factor: self.period_factor,
            regularization: self.regularization,
            rcond: self.rcond,
            padding: match self.padding {
                PaddingArg::Reflect => Padding::Reflect,
                PaddingArg::Symmetric => Padding::Symmetric,
                PaddingArg::Edge => Padding::Edge,
                PaddingArg::Wrap => Padding::Wrap,
                PaddingArg::Constant => Padding::Constant,
            },
            window: match self.window {
                WindowArg::Hann => Window::Hann,
                WindowArg::Lanczos => Window::Lanczos,
                WindowArg::None => Window::None,
            },
            table_interp: match self.table_interp {
                InterpArg::Cubic => TableInterp::Cubic,
                InterpArg::Linear => TableInterp::Linear,
            },
            normalize: !self.no_normalize,
            max_memory_bytes: budget,
        };
        validate_controls(&options)?;
        Ok(options)
    }
}

fn validate_controls(options: &ResizeOptions) -> Result<()> {
    for (name, value) in [
        ("support", options.support),
        ("density", options.density),
        ("period-factor", options.period_factor),
    ] {
        ensure!(
            value.is_finite() && value > 0.0,
            "{name} must be finite and positive"
        );
    }
    ensure!(
        options.density >= 1.0 && options.period_factor >= 2.0,
        "density must be at least 1; period-factor must be at least 2"
    );
    match options.camera {
        Camera::Gaussian { sigma } => ensure!(
            sigma.is_finite() && sigma > 0.0,
            "sigma must be finite and positive"
        ),
        Camera::IdealLowpass { cutoff } => ensure!(
            cutoff.is_finite() && cutoff > 0.0,
            "cutoff must be finite and positive"
        ),
        Camera::Cubic => {}
        _ => bail!("unsupported camera"),
    }
    for (name, value) in [("scale-x", options.scale_x), ("scale-y", options.scale_y)] {
        ensure!(
            !value.is_some_and(|scale| !scale.is_finite() || scale <= 0.0),
            "{name} must be finite and positive"
        );
    }
    ensure!(
        options.width != Some(0) && options.height != Some(0),
        "dimensions must be positive"
    );
    ensure!(
        options.regularization.is_finite()
            && options.regularization >= 0.0
            && options.rcond.is_finite()
            && options.rcond >= 0.0,
        "regularization and rcond must be finite and nonnegative"
    );
    Ok(())
}

fn decoded_input(path: &Path, budget: usize) -> Result<Image> {
    // Keep codec predecode limits conservative: HEIC derives its canvas budget
    // from this pixel limit. Account for the actual RGB/RGBA conversion below.
    let decode_bytes_per_pixel = 4 * size_of::<f64>() + 4;
    let decoded = codec::decode(path, budget / decode_bytes_per_pixel)?;
    let channels = match decoded.alpha {
        AlphaChannel::Absent => 3,
        AlphaChannel::Present => 4,
    };
    let pixel_count = decoded
        .width
        .checked_mul(decoded.height)
        .context("input dimensions overflow")?;
    let count = pixel_count
        .checked_mul(channels)
        .context("input dimensions overflow")?;
    let conversion_bytes = count
        .checked_mul(size_of::<f64>())
        .and_then(|bytes| bytes.checked_add(decoded.pixels.len()))
        .context("input size overflows")?;
    ensure!(
        conversion_bytes <= budget,
        "input conversion exceeds working-memory budget"
    );
    let mut data = Vec::new();
    data.try_reserve_exact(count)
        .context("cannot allocate float input")?;
    for rgba in decoded.pixels.chunks_exact(4) {
        if decoded.alpha == AlphaChannel::Present {
            let alpha = f64::from(rgba[3]) / 255.0;
            data.extend([
                f64::from(rgba[0]) / 255.0 * alpha,
                f64::from(rgba[1]) / 255.0 * alpha,
                f64::from(rgba[2]) / 255.0 * alpha,
                alpha,
            ]);
        } else {
            data.extend(rgba[..3].iter().map(|&value| f64::from(value) / 255.0));
        }
    }
    Ok(Image {
        width: decoded.width,
        height: decoded.height,
        channels,
        data,
    })
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::as_conversions,
    reason = "Final image quantization deliberately rounds a clamped 0..255 value to its integer code."
)]
fn quantized_code(value: f64) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn quantized_image(output: &Image, format: OutputFormat, budget: usize) -> Result<RgbaImage> {
    let alpha = match output.channels {
        3 => AlphaChannel::Absent,
        4 => AlphaChannel::Present,
        _ => bail!("output must have RGB or RGBA channels"),
    };
    let pixel_count = output
        .width
        .checked_mul(output.height)
        .context("output size overflows")?;
    let count = pixel_count
        .checked_mul(output.channels)
        .context("output size overflows")?;
    ensure!(count == output.data.len(), "output pixel count is invalid");
    let packed_count = pixel_count
        .checked_mul(4)
        .context("output size overflows")?;
    let conversion_bytes = count
        .checked_mul(size_of::<f64>())
        .and_then(|bytes| bytes.checked_add(packed_count))
        .context("output size overflows")?;
    ensure!(
        conversion_bytes <= budget,
        "output conversion exceeds working-memory budget"
    );
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(packed_count)
        .context("cannot allocate encoded pixel buffer")?;
    for pixel in output.data.chunks_exact(output.channels) {
        ensure!(
            pixel.iter().all(|value| value.is_finite()),
            "resize produced non-finite pixels; use regularization or different camera settings"
        );
        if alpha == AlphaChannel::Absent {
            pixels.extend(pixel.iter().map(|&value| quantized_code(value)));
            pixels.push(255);
            continue;
        }
        let alpha = pixel[3].clamp(0.0, 1.0);
        let alpha_byte = quantized_code(alpha);
        for channel in &pixel[..3] {
            let color = if format == OutputFormat::Png {
                if alpha_byte > 0 { channel / alpha } else { 0.0 }
            } else {
                channel + 1.0 - alpha
            };
            pixels.push(quantized_code(color));
        }
        pixels.push(if format == OutputFormat::Png {
            alpha_byte
        } else {
            255
        });
    }
    Ok(RgbaImage {
        width: output.width,
        height: output.height,
        pixels,
        alpha: if format == OutputFormat::Png {
            alpha
        } else {
            AlphaChannel::Absent
        },
    })
}

fn publish(image: &RgbaImage, args: &Args, format: OutputFormat, parent: &Path) -> Result<()> {
    let temp = tempfile::Builder::new()
        .prefix(".rro-")
        .tempfile_in(parent)
        .with_context(|| format!("cannot create output staging file in {}", parent.display()))?;
    codec::encode(image, temp.path(), format, args.quality)?;
    temp.as_file().sync_all().context("cannot flush output")?;
    reject_alias(&args.input, &args.output)?;
    if args.force {
        temp.persist(&args.output)
            .map_err(|error| error.error)
            .context("cannot publish output")?;
    } else {
        temp.persist_noclobber(&args.output)
            .map_err(|error| error.error)
            .context("cannot publish output without replacing an existing file")?;
    }
    Ok(())
}

fn run(args: &Args) -> Result<()> {
    let total_start = Instant::now();
    format_for(&args.input)?;
    let format = format_for(&args.output)?;
    let options = args.resize_options()?;
    reject_alias(&args.input, &args.output)?;
    ensure!(
        args.force || fs::symlink_metadata(&args.output).is_err(),
        "output already exists; use --force to replace it"
    );
    let parent = args
        .output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    ensure!(parent.is_dir(), "output directory does not exist");
    let decode_start = Instant::now();
    let input = decoded_input(&args.input, options.max_memory_bytes)?;
    let decode_time = decode_start.elapsed();
    let mut pool_builder = rayon::ThreadPoolBuilder::new();
    if let Some(threads) = args.threads {
        pool_builder = pool_builder.num_threads(threads);
    }
    let pool = pool_builder.build().context("cannot start workers")?;
    let resize_start = Instant::now();
    let output = pool.install(|| core::resize(&input, &options))?;
    let resize_time = resize_start.elapsed();
    drop(input);
    let image = quantized_image(&output, format, options.max_memory_bytes)?;
    drop(output);
    let encode_start = Instant::now();
    publish(&image, args, format, parent)?;
    let encode_time = encode_start.elapsed();
    if args.timings {
        bin_support::diagnostic(format_args!(
            "{}x{}; decode {:.3}s, resize {:.3}s, encode {:.3}s, total {:.3}s",
            image.width,
            image.height,
            decode_time.as_secs_f64(),
            resize_time.as_secs_f64(),
            encode_time.as_secs_f64(),
            total_start.elapsed().as_secs_f64(),
        ));
    }
    Ok(())
}

fn main() -> ExitCode {
    let result = bin_support::initialize_logging().and_then(|()| run(&Args::parse()));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            bin_support::diagnostic(format_args!("rro: {error:#}"));
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Image, OutputFormat, decoded_input, quantized_image};

    #[test]
    fn decoder_rejects_over_budget_source_dimensions_before_float_conversion() {
        use rro::codec::{self, AlphaChannel, RgbaImage};

        let work = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("reports/work");
        std::fs::create_dir_all(&work).expect("test workspace is writable");
        let directory = tempfile::Builder::new()
            .prefix("decode-budget-test-")
            .tempdir_in(work)
            .expect("test workspace accepts temporary directories");
        let path = directory.path().join("rgb.png");
        codec::encode(
            &RgbaImage {
                width: 256,
                height: 256,
                pixels: [17, 23, 41, 255].repeat(256 * 256),
                alpha: AlphaChannel::Absent,
            },
            &path,
            OutputFormat::Png,
            0.9,
        )
        .expect("valid RGB fixture can be encoded");
        let error = decoded_input(&path, 2 * 1024 * 1024)
            .expect_err("source dimensions must be rejected at the codec boundary");
        assert!(
            error
                .to_string()
                .contains("input image exceeds pixel/memory limit"),
            "decode must reject dimensions before float conversion: {error:#}"
        );
        assert!(decoded_input(&path, 4 * 1024 * 1024).is_ok());
    }

    #[test]
    fn quantization_budget_includes_float_and_packed_rgba_storage() {
        for channels in [3, 4] {
            let output = Image {
                width: 1,
                height: 1,
                channels,
                data: vec![1.0; channels],
            };
            let bytes = channels * size_of::<f64>() + 4;
            assert!(
                quantized_image(&output, OutputFormat::Png, bytes - 1).is_err(),
                "{channels}-channel conversion must account for all live storage"
            );
            assert!(
                quantized_image(&output, OutputFormat::Png, bytes).is_ok(),
                "{channels}-channel conversion fits its exact buffer budget"
            );
        }
    }

    #[test]
    fn quantization_rejects_invalid_shape_channels_and_nonfinite_pixels() {
        for (channels, data) in [
            (1, vec![0.5]),
            (5, vec![0.5; 5]),
            (3, vec![0.5; 2]),
            (3, vec![0.0, f64::NAN, 0.0]),
            (4, vec![0.0, 0.0, 0.0, f64::INFINITY]),
        ] {
            let output = Image {
                width: 1,
                height: 1,
                channels,
                data,
            };
            assert!(
                quantized_image(&output, OutputFormat::Png, 1000).is_err(),
                "reject invalid quantization input: {output:?}"
            );
        }
    }
}
