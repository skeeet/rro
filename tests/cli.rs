//! File-conversion behavior and publication safety at the command-line boundary.
use rro::codec::{self, AlphaChannel, OutputFormat, RgbaImage};
use std::fs;
use std::path::Path;
use std::process::{Command, ExitStatus, Output};

fn workspace() -> tempfile::TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("reports/work");
    fs::create_dir_all(&root).expect("test workspace is writable");
    tempfile::Builder::new()
        .prefix("cli-test-")
        .tempdir_in(root)
        .expect("test workspace accepts temporary directories")
}
fn cli(input: &Path, output: &Path, flags: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rro"))
        .arg(input)
        .arg(output)
        .args(flags)
        .output()
        .expect("test executable can be launched")
}
fn fixture(path: &Path) {
    let (width, height) = (11, 7);
    let mut pixels = Vec::new();
    for y in 0..height {
        for x in 0..width {
            pixels.extend([
                u8::try_from(x * 19).expect("fixture red fits u8"),
                u8::try_from(y * 31).expect("fixture green fits u8"),
                u8::try_from((x + y) * 13).expect("fixture blue fits u8"),
                255,
            ]);
        }
    }
    codec::encode(
        &RgbaImage {
            width,
            height,
            pixels,
            alpha: AlphaChannel::Absent,
        },
        path,
        OutputFormat::Png,
        0.9,
    )
    .expect("generated PNG fixture can be encoded");
}
fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
}

fn write_png(path: &Path, color: png::ColorType, pixels: &[u8]) {
    let file = fs::File::create(path).expect("PNG fixture can be created");
    let mut encoder = png::Encoder::new(file, 4, 3);
    encoder.set_color(color);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .expect("PNG fixture header can be written")
        .write_image_data(pixels)
        .expect("PNG fixture pixels can be written");
}

fn png_color_type(path: &Path) -> u8 {
    let bytes = fs::read(path).expect("output PNG can be read");
    assert!(bytes.len() >= 26, "output contains a PNG IHDR");
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "PNG signature");
    assert_eq!(&bytes[12..16], b"IHDR", "first PNG chunk is IHDR");
    bytes[25]
}

#[test]
fn png_output_omits_alpha_for_rgb_and_grayscale_sources() {
    let dir = workspace();
    for (name, color, pixels) in [
        ("rgb", png::ColorType::Rgb, [40, 80, 120].repeat(12)),
        ("gray", png::ColorType::Grayscale, vec![80; 12]),
    ] {
        let input = dir.path().join(format!("{name}.png"));
        write_png(&input, color, &pixels);
        for (label, flags) in [
            ("identity", vec!["--scale", "1"]),
            (
                "resized",
                vec!["--scale", "1.5", "--padding", "constant", "--no-normalize"],
            ),
        ] {
            let output = dir.path().join(format!("{name}-{label}.png"));
            assert_success(&cli(&input, &output, &flags));
            assert_eq!(png_color_type(&output), 2, "{name} {label}: RGB PNG");
            let decoded = codec::decode(&output, 1000).unwrap();
            assert_eq!(decoded.alpha, AlphaChannel::Absent, "{name} {label}");
            assert!(decoded.pixels.chunks_exact(4).all(|pixel| pixel[3] == 255));
        }
    }
}

#[test]
fn png_output_retains_opaque_and_transparent_alpha_channels() {
    let dir = workspace();
    for (name, color, pixels) in [
        (
            "opaque-rgba",
            png::ColorType::Rgba,
            [40, 80, 120, 255].repeat(12),
        ),
        (
            "transparent-rgba",
            png::ColorType::Rgba,
            [40, 80, 120, 0, 50, 100, 150, 128, 60, 120, 180, 255].repeat(4),
        ),
        (
            "opaque-gray-alpha",
            png::ColorType::GrayscaleAlpha,
            [80, 255].repeat(12),
        ),
    ] {
        let input = dir.path().join(format!("{name}.png"));
        let output = dir.path().join(format!("{name}-output.png"));
        write_png(&input, color, &pixels);
        assert_success(&cli(&input, &output, &["--scale", "1"]));
        assert_eq!(png_color_type(&output), 6, "{name}: RGBA PNG");
        let expected = codec::decode(&input, 1000).unwrap();
        let actual = codec::decode(&output, 1000).unwrap();
        assert_eq!(actual.alpha, AlphaChannel::Present, "{name}");
        assert_eq!(actual.pixels, expected.pixels, "{name}: unit-scale pixels");
    }
}

#[test]
fn jpeg_and_heic_inputs_produce_rgb_png() {
    let dir = workspace();
    let source = dir.path().join("source.png");
    fixture(&source);
    for extension in ["jpg", "heic"] {
        let input = dir.path().join(format!("input.{extension}"));
        let output = dir.path().join(format!("from-{extension}.png"));
        assert_success(&cli(&source, &input, &["--scale", "1"]));
        assert_success(&cli(&input, &output, &["--scale", "0.5"]));
        assert_eq!(png_color_type(&output), 2, "{extension}: RGB PNG");
        assert_eq!(
            codec::decode(&output, 1000).unwrap().alpha,
            AlphaChannel::Absent
        );
    }
}

#[test]
fn rgb_constant_padding_matches_three_channel_core_without_synthetic_alpha() {
    use rro::core::{self, Camera, Image, Padding, ResizeOptions};

    let dir = workspace();
    let input = dir.path().join("rgb.png");
    let output = dir.path().join("output.png");
    let pixels = [40, 80, 120].repeat(12);
    write_png(&input, png::ColorType::Rgb, &pixels);
    let source = Image {
        width: 4,
        height: 3,
        channels: 3,
        data: pixels
            .iter()
            .map(|&value| f64::from(value) / 255.0)
            .collect(),
    };
    let options = ResizeOptions {
        scale_x: Some(1.5),
        scale_y: Some(1.5),
        camera: Camera::Gaussian { sigma: 0.7 },
        padding: Padding::Constant,
        normalize: false,
        ..ResizeOptions::default()
    };
    let expected = core::resize(&source, &options).unwrap();
    assert_success(&cli(
        &input,
        &output,
        &[
            "--scale",
            "1.5",
            "--camera",
            "gaussian",
            "--padding",
            "constant",
            "--no-normalize",
        ],
    ));
    assert_eq!(png_color_type(&output), 2, "constant padding retains RGB");
    let actual = codec::decode(&output, 1000).unwrap();
    assert_eq!(
        (actual.width, actual.height),
        (expected.width, expected.height)
    );
    for (actual, expected) in actual
        .pixels
        .chunks_exact(4)
        .zip(expected.data.chunks_exact(3))
    {
        for (&code, &value) in actual[..3].iter().zip(expected) {
            let difference = (f64::from(code) - (value.clamp(0.0, 1.0) * 255.0).round()).abs();
            assert!(
                difference <= 1.0,
                "RGB code {code} differs from direct-core value {value}"
            );
        }
        assert_eq!(actual[3], 255, "no synthetic alpha channel");
    }
}

#[test]
fn geometry_roundtrip_and_unicode_paths() {
    let dir = workspace();
    let input = dir.path().join("source café 日本.png");
    let output = dir.path().join("output spaced.png");
    fixture(&input);
    let before = fs::read(&input).unwrap();
    assert_success(&cli(&input, &output, &["--scale", "1"]));
    assert_eq!(
        codec::decode(&input, 1000).unwrap(),
        codec::decode(&output, 1000).unwrap()
    );
    assert_eq!(before, fs::read(&input).unwrap());
    assert_success(&cli(&input, &output, &["--width", "5", "--force"]));
    let image = codec::decode(&output, 1000).unwrap();
    assert_eq!((image.width, image.height), (5, 4));
    assert_success(&cli(
        &input,
        &output,
        &[
            "--scale-x",
            "0.5",
            "--scale-y",
            "0.75",
            "--width",
            "8",
            "--height",
            "9",
            "--force",
        ],
    ));
    let image = codec::decode(&output, 1000).unwrap();
    assert_eq!((image.width, image.height), (8, 9));
}

#[test]
fn alias_and_overwrite_guards_preserve_source() {
    let dir = workspace();
    let input = dir.path().join("source.png");
    let output = dir.path().join("dest.png");
    fixture(&input);
    let before = fs::read(&input).unwrap();
    fs::write(&output, b"keep destination").unwrap();
    assert!(!cli(&input, &output, &["--scale", "0.5"]).status.success());
    assert_eq!(fs::read(&output).unwrap(), b"keep destination");
    assert!(
        !cli(
            &input,
            &output,
            &[
                "--width",
                "18446744073709551615",
                "--height",
                "18446744073709551615",
                "--force"
            ]
        )
        .status
        .success()
    );
    assert_eq!(fs::read(&output).unwrap(), b"keep destination");
    assert!(
        !cli(&input, &input, &["--scale", "0.5", "--force"])
            .status
            .success()
    );
    let hard = dir.path().join("hard.png");
    fs::hard_link(&input, &hard).unwrap();
    assert!(
        !cli(&input, &hard, &["--scale", "0.5", "--force"])
            .status
            .success()
    );
    #[cfg(unix)]
    {
        let sym = dir.path().join("symlink.png");
        std::os::unix::fs::symlink(&input, &sym).unwrap();
        assert!(
            !cli(&input, &sym, &["--scale", "0.5", "--force"])
                .status
                .success()
        );
    }
    assert_eq!(before, fs::read(&input).unwrap());
    assert_success(&cli(&input, &output, &["--scale", "0.5", "--force"]));
    assert_eq!(codec::decode(&output, 1000).unwrap().width, 6);
    assert!(!fs::read_dir(dir.path()).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".rro-")
    }));
}

#[test]
fn no_clobber_race_has_one_winner() {
    let dir = workspace();
    let input = dir.path().join("source.png");
    let output = dir.path().join("dest.png");
    fixture(&input);
    let mut children = Vec::new();
    for _ in 0..8 {
        children.push(
            Command::new(env!("CARGO_BIN_EXE_rro"))
                .arg(&input)
                .arg(&output)
                .args(["--scale", "0.5", "--threads", "1"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
    }
    let wins = children
        .into_iter()
        .map(|mut child| {
            child
                .wait()
                .expect("spawned conversion process can be waited for")
        })
        .filter(ExitStatus::success)
        .count();
    assert_eq!(wins, 1);
    assert_eq!(codec::decode(&output, 1000).unwrap().width, 6);
}

#[test]
fn invalid_controls_and_corrupt_files_do_not_publish() {
    let dir = workspace();
    let input = dir.path().join("source.png");
    let output = dir.path().join("out.png");
    fixture(&input);
    for args in [
        vec!["--scale", "NaN"],
        vec!["--scale", "0"],
        vec!["--width", "0"],
        vec!["--scale", "0.5", "--width", "3"],
        vec!["--scale-x", "0.5"],
        vec!["--scale", "0.5", "--sigma", "1"],
        vec!["--scale", "0.5", "--threads", "999999"],
        vec!["--scale", "0.5", "--density", "0.5"],
        vec!["--scale", "0.5", "--quality", "1.5"],
        vec!["--scale", "0.5", "--max-memory-mib", "0"],
        vec!["--scale", "0.5", "--camera", "gaussian", "--sigma", "0"],
    ] {
        assert!(!cli(&input, &output, &args).status.success(), "{args:?}");
        assert!(!output.exists(), "{args:?}");
    }
    fs::write(&input, b"not an image").unwrap();
    assert!(!cli(&input, &output, &["--scale", "0.5"]).status.success());
    assert!(!output.exists());
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn help_and_version_are_available_without_files() {
    let help = Command::new(env!("CARGO_BIN_EXE_rro"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("--scale"));
    let version = Command::new(env!("CARGO_BIN_EXE_rro"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(version.status.success());
    assert!(
        String::from_utf8_lossy(&version.stdout)
            .contains(concat!("rro ", env!("CARGO_PKG_VERSION")))
    );
}

#[test]
fn failed_publication_cleans_staging_file() {
    let dir = workspace();
    let input = dir.path().join("source.png");
    let output = dir.path().join("directory.png");
    fixture(&input);
    fs::create_dir(&output).unwrap();
    fs::write(output.join("marker"), b"preserve this directory").unwrap();
    assert!(
        !cli(&input, &output, &["--scale", "0.5", "--force"])
            .status
            .success()
    );
    assert_eq!(
        fs::read(output.join("marker")).unwrap(),
        b"preserve this directory"
    );
    assert!(!fs::read_dir(dir.path()).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".rro-")
    }));
}

#[test]
fn fully_transparent_output_pixels_have_zero_rgb() {
    let dir = workspace();
    let input = dir.path().join("alpha-edge.png");
    let output = dir.path().join("alpha-result.png");
    let (width, height) = (61, 43);
    let mut pixels = Vec::new();
    for _y in 0..height {
        for x in 0..width {
            let alpha = if x < 20 {
                0
            } else if x < 40 {
                255
            } else {
                u8::try_from((x - 40) * 12).expect("fixture alpha fits u8")
            };
            pixels.extend(if x < 20 {
                [255, 0, 255, alpha]
            } else {
                [0, 220, 40, alpha]
            });
        }
    }
    codec::encode(
        &RgbaImage {
            width,
            height,
            pixels,
            alpha: AlphaChannel::Present,
        },
        &input,
        OutputFormat::Png,
        0.9,
    )
    .unwrap();
    assert_success(&cli(
        &input,
        &output,
        &["--scale", "1", "--camera", "gaussian"],
    ));
    let decoded = codec::decode(&output, 10000).unwrap();
    let mut transparent_count = 0;
    for rgba in decoded.pixels.chunks_exact(4) {
        if rgba[3] == 0 {
            transparent_count += 1;
            assert_eq!(&rgba[..3], &[0, 0, 0]);
        }
    }
    assert!(transparent_count > 0);
}

#[test]
fn numeric_helper_rejects_input_and_request_aliases() {
    let dir = workspace();
    let input = dir.path().join("input.f64");
    let request_path = dir.path().join("request.json");
    let source = 0.5_f64.to_le_bytes();
    for destination in [&input, &request_path] {
        fs::write(&input, source).unwrap();
        let request = serde_json::json!({
            "input": input, "output": destination,
            "width": 1, "height": 1, "channels": 1,
            "options": { "scale_x": 1.0, "scale_y": 1.0 },
            "threads": 1,
        });
        let request_bytes = serde_json::to_vec(&request).unwrap();
        fs::write(&request_path, &request_bytes).unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_rro-numeric"))
            .arg(&request_path)
            .output()
            .unwrap();
        assert!(
            !result.status.success(),
            "numeric helper accepted destination alias {}",
            destination.display()
        );
        assert_eq!(fs::read(&input).unwrap(), source, "raw input preserved");
        assert_eq!(
            fs::read(&request_path).unwrap(),
            request_bytes,
            "request preserved"
        );
    }
}

#[test]
fn numeric_helper_replaces_output_atomically_and_bounds_requests() {
    let dir = workspace();
    let input = dir.path().join("input.f64");
    let output = dir.path().join("output.f64");
    let request_path = dir.path().join("request.json");
    let source = 0.5_f64.to_le_bytes();
    fs::write(&input, source).unwrap();
    let mut request = serde_json::json!({
        "input": input, "output": output,
        "width": 1, "height": 1, "channels": 1,
        "options": { "scale_x": 1.0, "scale_y": 1.0 },
        "threads": 1,
    });
    fs::write(&request_path, serde_json::to_vec(&request).unwrap()).unwrap();
    fs::write(&output, b"old output").unwrap();
    let invoke = || {
        Command::new(env!("CARGO_BIN_EXE_rro-numeric"))
            .arg(&request_path)
            .output()
            .unwrap()
    };
    let result = invoke();
    assert!(result.status.success(), "{:?}", result.stderr);
    let bytes: [u8; 8] = fs::read(&output).unwrap().try_into().unwrap();
    assert!((f64::from_le_bytes(bytes) - 0.5).abs() < 1e-12);
    assert_eq!(fs::read(&input).unwrap(), source);

    fs::write(&output, b"keep output").unwrap();
    request["width"] = serde_json::json!(2);
    fs::write(&request_path, serde_json::to_vec(&request).unwrap()).unwrap();
    assert!(!invoke().status.success(), "shape mismatch rejected");
    assert_eq!(fs::read(&output).unwrap(), b"keep output");

    fs::write(&request_path, vec![b' '; 1_048_577]).unwrap();
    let result = invoke();
    assert!(!result.status.success(), "oversized request rejected");
    assert!(String::from_utf8_lossy(&result.stderr).contains("1 MiB"));
    assert_eq!(fs::read(&output).unwrap(), b"keep output");
    assert!(fs::read_dir(dir.path()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".rro-numeric-")
    }));
}
