//! Development-only raw float64 interchange and in-process resize timing.
#[path = "../bin_support/mod.rs"]
mod bin_support;

use anyhow::{Context, Result, bail, ensure};
use rro::core::{self, Camera, Image, Padding, ResizeOptions, TableInterp, Window};
use serde::Deserialize;
use serde_json::json;
use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

/// Bound development request metadata independently of the image memory budget.
const MAX_REQUEST_BYTES: usize = 1_048_576;

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum CameraOption {
    Cubic,
    Gaussian,
    IdealLowpass,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum PaddingOption {
    Reflect,
    Symmetric,
    Edge,
    Wrap,
    Constant,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum WindowOption {
    Hann,
    Lanczos,
    None,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum InterpolationOption {
    Cubic,
    Linear,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Options {
    scale_x: Option<f64>,
    scale_y: Option<f64>,
    width: Option<usize>,
    height: Option<usize>,
    camera: Option<CameraOption>,
    sigma: Option<f64>,
    cutoff: Option<f64>,
    support: Option<f64>,
    density: Option<f64>,
    period_factor: Option<f64>,
    regularization: Option<f64>,
    rcond: Option<f64>,
    padding: Option<PaddingOption>,
    window: Option<WindowOption>,
    table_interp: Option<InterpolationOption>,
    normalize: Option<bool>,
    max_memory_bytes: Option<usize>,
}

impl Options {
    fn into_resize_options(self) -> ResizeOptions {
        let defaults = ResizeOptions::default();
        ResizeOptions {
            scale_x: self.scale_x,
            scale_y: self.scale_y,
            width: self.width,
            height: self.height,
            camera: match self.camera.unwrap_or(CameraOption::Cubic) {
                CameraOption::Cubic => Camera::Cubic,
                CameraOption::Gaussian => Camera::Gaussian {
                    sigma: self.sigma.unwrap_or(0.7),
                },
                CameraOption::IdealLowpass => Camera::IdealLowpass {
                    cutoff: self.cutoff.unwrap_or(std::f64::consts::PI),
                },
            },
            support: self.support.unwrap_or(defaults.support),
            density: self.density.unwrap_or(defaults.density),
            period_factor: self.period_factor.unwrap_or(defaults.period_factor),
            regularization: self.regularization.unwrap_or(defaults.regularization),
            rcond: self.rcond.unwrap_or(defaults.rcond),
            normalize: self.normalize.unwrap_or(defaults.normalize),
            max_memory_bytes: self.max_memory_bytes.unwrap_or(defaults.max_memory_bytes),
            padding: match self.padding {
                None => defaults.padding,
                Some(PaddingOption::Reflect) => Padding::Reflect,
                Some(PaddingOption::Symmetric) => Padding::Symmetric,
                Some(PaddingOption::Edge) => Padding::Edge,
                Some(PaddingOption::Wrap) => Padding::Wrap,
                Some(PaddingOption::Constant) => Padding::Constant,
            },
            window: match self.window {
                None => defaults.window,
                Some(WindowOption::Hann) => Window::Hann,
                Some(WindowOption::Lanczos) => Window::Lanczos,
                Some(WindowOption::None) => Window::None,
            },
            table_interp: match self.table_interp {
                None => defaults.table_interp,
                Some(InterpolationOption::Cubic) => TableInterp::Cubic,
                Some(InterpolationOption::Linear) => TableInterp::Linear,
            },
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    input: PathBuf,
    output: PathBuf,
    width: usize,
    height: usize,
    channels: usize,
    #[serde(default)]
    options: Options,
    #[serde(default = "one")]
    repeats: usize,
    #[serde(default)]
    warmup: usize,
    threads: Option<usize>,
}

fn one() -> usize {
    1
}

fn reject_alias(source: &Path, output: &Path) -> Result<()> {
    match same_file::is_same_file(source, output) {
        Ok(true) => bail!(
            "numeric output aliases protected source {}",
            source.display()
        ),
        Ok(false) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("cannot compare numeric output with protected source"),
    }
}

fn open_regular(path: &Path) -> Result<File> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("cannot inspect numeric input {}", path.display()))?;
    ensure!(metadata.is_file(), "numeric input must be a regular file");
    let file = File::open(path)
        .with_context(|| format!("cannot open numeric input {}", path.display()))?;
    ensure!(
        file.metadata()
            .context("cannot inspect open numeric input")?
            .is_file(),
        "numeric input must be a regular file"
    );
    Ok(file)
}

fn read_payload(reader: &mut impl Read, expected: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(expected)
        .context("cannot allocate bounded numeric input")?;
    bytes.resize(expected, 0);
    reader
        .read_exact(&mut bytes)
        .context("numeric input shrank while reading")?;
    let mut trailing = [0];
    ensure!(
        reader
            .read(&mut trailing)
            .context("cannot finish numeric input")?
            == 0,
        "numeric input grew while reading"
    );
    Ok(bytes)
}

fn read_image(request: &Request, memory_budget: usize) -> Result<Image> {
    let count = request
        .width
        .checked_mul(request.height)
        .and_then(|value| value.checked_mul(request.channels))
        .context("input shape overflows")?;
    let bytes_count = count.checked_mul(8).context("input size overflows")?;
    ensure!(
        bytes_count <= memory_budget / 2,
        "input interchange exceeds memory budget"
    );
    let expected = u64::try_from(bytes_count).context("input length cannot be represented")?;
    let mut file = open_regular(&request.input)?;
    let metadata = file.metadata().context("cannot inspect open raw input")?;
    ensure!(
        metadata.len() == expected,
        "raw input length does not match shape"
    );
    // Read only the validated size from the same file handle. A concurrently
    // replaced or growing path cannot turn this into an unbounded allocation.
    let bytes = read_payload(&mut file, bytes_count)?;
    let mut data = Vec::new();
    data.try_reserve_exact(count)
        .context("cannot allocate raw input")?;
    for chunk in bytes.chunks_exact(8) {
        let value = chunk
            .try_into()
            .context("raw input contains an incomplete float64 value")?;
        data.push(f64::from_le_bytes(value));
    }
    Ok(Image {
        width: request.width,
        height: request.height,
        channels: request.channels,
        data,
    })
}

fn execute(request: Request) -> Result<serde_json::Value> {
    ensure!(
        request.repeats > 0 && request.repeats <= 1000 && request.warmup <= 1000,
        "repeats must be 1..1000; warmup must be 0..1000"
    );
    ensure!(
        !request
            .threads
            .is_some_and(|count| count == 0 || count > 256),
        "threads must be between 1 and 256"
    );
    reject_alias(&request.input, &request.output)?;
    // Image loading borrows the request before consuming its option record.
    let budget = request
        .options
        .max_memory_bytes
        .unwrap_or_else(|| ResizeOptions::default().max_memory_bytes);
    let image = read_image(&request, budget)?;
    let options = request.options.into_resize_options();
    let mut builder = rayon::ThreadPoolBuilder::new();
    if let Some(threads) = request.threads {
        builder = builder.num_threads(threads);
    }
    let pool = builder.build().context("cannot start numeric workers")?;
    let mut timings = Vec::new();
    for _ in 0..request.warmup {
        drop(pool.install(|| core::resize(&image, &options))?);
    }
    let mut last = None;
    for _ in 0..request.repeats {
        // Exclude disposal of the preceding result from the next resize timing.
        drop(last.take());
        let start = Instant::now();
        let result = pool.install(|| core::resize(&image, &options))?;
        timings.push(start.elapsed().as_secs_f64() * 1000.0);
        last = Some(result);
    }
    let result = last.context("numeric benchmark completed without a result")?;
    let parent = request
        .output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut staging = tempfile::Builder::new()
        .prefix(".rro-numeric-")
        .tempfile_in(parent)
        .context("cannot create numeric staging file")?;
    {
        let mut writer = BufWriter::new(staging.as_file_mut());
        for value in &result.data {
            writer
                .write_all(&value.to_le_bytes())
                .context("cannot write numeric result")?;
        }
        writer.flush().context("cannot flush numeric result")?;
    }
    staging
        .as_file()
        .sync_all()
        .context("cannot sync numeric result")?;
    reject_alias(&request.input, &request.output)?;
    staging
        .persist(&request.output)
        .map_err(|error| error.error)
        .context("cannot publish numeric result")?;
    Ok(json!({
        "width": result.width, "height": result.height, "channels": result.channels,
        "samples_ms": timings, "threads": pool.current_num_threads(),
    }))
}

fn run() -> Result<()> {
    let Some(path) = std::env::args_os().nth(1) else {
        bail!("usage: rro-numeric request.json");
    };
    let mut file = open_regular(Path::new(&path))?;
    let metadata = file.metadata().context("cannot inspect numeric request")?;
    let length = usize::try_from(metadata.len()).context("numeric request length overflows")?;
    ensure!(
        length <= MAX_REQUEST_BYTES,
        "numeric request exceeds 1 MiB limit"
    );
    let bytes = read_payload(&mut file, length).context("cannot read numeric request")?;
    let request: Request = serde_json::from_slice(&bytes).context("invalid numeric request")?;
    reject_alias(Path::new(&path), &request.output)?;
    let payload = execute(request)?;
    let mut output = io::stdout().lock();
    serde_json::to_writer(&mut output, &payload).context("cannot write numeric response")?;
    output
        .write_all(b"\n")
        .context("cannot finish numeric response")?;
    Ok(())
}

fn main() -> ExitCode {
    match bin_support::initialize_logging().and_then(|()| run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            bin_support::diagnostic(format_args!("rro-numeric: {error:#}"));
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::read_payload;
    use std::io::Cursor;

    #[test]
    fn bounded_payload_rejects_shrink_and_growth() {
        let mut exact = Cursor::new(vec![1, 2, 3]);
        assert_eq!(read_payload(&mut exact, 3).unwrap(), vec![1, 2, 3]);

        let mut shrunk = Cursor::new(vec![1, 2]);
        assert!(read_payload(&mut shrunk, 3).is_err(), "truncation rejected");

        let mut grown = Cursor::new(vec![1, 2, 3, 4]);
        let error = read_payload(&mut grown, 3).expect_err("growth must be rejected");
        assert!(
            error.to_string().contains("grew"),
            "growth context preserved"
        );

        let mut empty = Cursor::new(Vec::<u8>::new());
        assert!(
            read_payload(&mut empty, usize::MAX).is_err(),
            "impossible capacities return errors instead of panicking"
        );
    }
}
