//! Independent pixel-coordinate checks for input orientation metadata and normalization.
use image::{ImageDecoder, ImageEncoder};
use oxideav_heif::encode::TransformCarrier;
use oxideav_heif::{EncodeOptions, HeifFile, HeifWriter, Imir, Irot, Ispe, Property};
use rro::codec::{self, RgbaImage};
use std::fs::{self, File};
use std::path::Path;
use std::process::Command;

const WIDTH: usize = 12;
const HEIGHT: usize = 8;

#[derive(Clone, Copy, Debug)]
enum ByteOrder {
    Little,
    Big,
}

fn workspace() -> tempfile::TempDir {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("reports/work");
    fs::create_dir_all(&root).expect("orientation workspace is writable");
    tempfile::Builder::new()
        .prefix("orientation-")
        .tempdir_in(root)
        .expect("orientation workspace accepts temporary directories")
}

fn pixels() -> Vec<u8> {
    let mut rgb = Vec::new();
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            rgb.extend([
                u8::try_from(25 + x * 13).expect("fixture red fits u8"),
                u8::try_from(30 + y * 21).expect("fixture green fits u8"),
                u8::try_from(40 + x * 5 + y * 7).expect("fixture blue fits u8"),
            ]);
        }
    }
    rgb
}

fn tiff(orientation: u8, byte_order: ByteOrder) -> Vec<u8> {
    if matches!(byte_order, ByteOrder::Big) {
        let mut bytes = b"MM\0\x2a\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01".to_vec();
        bytes.extend_from_slice(&[0, orientation, 0, 0, 0, 0, 0, 0]);
        bytes
    } else {
        let mut bytes = b"II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
        bytes.extend_from_slice(&[orientation, 0, 0, 0, 0, 0, 0, 0]);
        bytes
    }
}

fn write_png(path: &Path, exif: Option<Vec<u8>>) {
    let mut info = png::Info::with_size(
        u32::try_from(WIDTH).expect("fixture width fits u32"),
        u32::try_from(HEIGHT).expect("fixture height fits u32"),
    );
    info.color_type = png::ColorType::Rgb;
    info.exif_metadata = exif.map(Into::into);
    let mut writer = png::Encoder::with_info(File::create(path).expect("create PNG fixture"), info)
        .expect("valid PNG fixture metadata")
        .write_header()
        .expect("write PNG header");
    writer
        .write_image_data(&pixels())
        .expect("write PNG pixels");
    writer.finish().expect("finish PNG fixture");
}

fn jpeg_bytes() -> Vec<u8> {
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 100)
        .write_image(
            &pixels(),
            u32::try_from(WIDTH).expect("fixture width fits u32"),
            u32::try_from(HEIGHT).expect("fixture height fits u32"),
            image::ExtendedColorType::Rgb8,
        )
        .expect("encode independent JPEG fixture");
    bytes
}

fn write_jpeg(path: &Path, bytes: &[u8], orientation: u8, byte_order: ByteOrder) {
    let mut exif = b"Exif\0\0".to_vec();
    exif.extend(tiff(orientation, byte_order));
    let mut output = bytes[..2].to_vec();
    output.extend_from_slice(&[0xff, 0xe1]);
    output.extend_from_slice(
        &u16::try_from(exif.len() + 2)
            .expect("fixture APP1 fits u16")
            .to_be_bytes(),
    );
    output.extend(exif);
    output.extend_from_slice(&bytes[2..]);
    fs::write(path, output).expect("write JPEG orientation fixture");
}

fn assert_oriented(actual: &RgbaImage, baseline: &RgbaImage, orientation: u8) {
    let (width, height) = (baseline.width, baseline.height);
    let expected_size = if orientation >= 5 {
        (height, width)
    } else {
        (width, height)
    };
    assert_eq!(
        (actual.width, actual.height),
        expected_size,
        "orientation {orientation} dimensions"
    );
    for y in 0..actual.height {
        for x in 0..actual.width {
            let (source_x, source_y) = match orientation {
                1 => (x, y),
                2 => (width - 1 - x, y),
                3 => (width - 1 - x, height - 1 - y),
                4 => (x, height - 1 - y),
                5 => (y, x),
                6 => (y, height - 1 - x),
                7 => (width - 1 - y, height - 1 - x),
                8 => (width - 1 - y, x),
                _ => unreachable!("test orientations are in 1..=8"),
            };
            assert_eq!(
                &actual.pixels[(y * actual.width + x) * 4..][..4],
                &baseline.pixels[(source_y * width + source_x) * 4..][..4],
                "orientation {orientation}, destination ({x},{y})"
            );
        }
    }
}

fn cli(source: &Path, output: &Path, flags: &[&str]) {
    let result = Command::new(env!("CARGO_BIN_EXE_rro"))
        .arg(source)
        .arg(output)
        .args(flags)
        .output()
        .expect("run conversion");
    assert!(
        result.status.success(),
        "conversion failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(result.stdout.is_empty(), "successful conversion stdout");
}

fn assert_normalized_png(path: &Path, expected: &RgbaImage) {
    let decoder = image::codecs::png::PngDecoder::new(std::io::BufReader::new(
        File::open(path).expect("open normalized PNG"),
    ))
    .expect("read normalized PNG");
    let mut decoder = decoder;
    assert!(
        decoder.exif_metadata().expect("read output EXIF").is_none(),
        "normalized output must not carry stale orientation"
    );
    assert_eq!(
        codec::decode(path, WIDTH * HEIGHT).expect("decode normalized PNG"),
        *expected,
        "normalized PNG decode must not rotate pixels again"
    );
}

#[test]
fn png_and_jpeg_exif_all_eight_normalize_once_in_both_byte_orders() {
    let work = workspace();
    let jpeg = jpeg_bytes();
    for extension in ["png", "jpg"] {
        let baseline_path = work.path().join(format!("baseline.{extension}"));
        if extension == "png" {
            write_png(&baseline_path, None);
        } else {
            fs::write(&baseline_path, &jpeg).expect("write baseline JPEG");
        }
        let baseline = codec::decode(&baseline_path, WIDTH * HEIGHT).expect("decode baseline");
        for byte_order in [ByteOrder::Little, ByteOrder::Big] {
            for orientation in 1..=8 {
                let source = work
                    .path()
                    .join(format!("input-{byte_order:?}-{orientation}.{extension}"));
                if extension == "png" {
                    write_png(&source, Some(tiff(orientation, byte_order)));
                } else {
                    write_jpeg(&source, &jpeg, orientation, byte_order);
                }
                let decoded =
                    codec::decode(&source, WIDTH * HEIGHT).expect("decode oriented input");
                assert_oriented(&decoded, &baseline, orientation);
                let output = work.path().join(format!(
                    "output-{extension}-{byte_order:?}-{orientation}.png"
                ));
                cli(&source, &output, &["--scale", "1"]);
                assert_normalized_png(&output, &decoded);
            }
        }
    }
}

fn transforms(orientation: u8) -> Vec<Property> {
    match orientation {
        1 => vec![],
        2 => vec![Property::Imir(Imir::new(1))],
        3 => vec![Property::Irot(Irot::new(2))],
        4 => vec![Property::Imir(Imir::new(0))],
        5 => vec![Property::Irot(Irot::new(3)), Property::Imir(Imir::new(1))],
        6 => vec![Property::Irot(Irot::new(3))],
        7 => vec![Property::Irot(Irot::new(3)), Property::Imir(Imir::new(0))],
        8 => vec![Property::Irot(Irot::new(1))],
        _ => unreachable!("test orientations are in 1..=8"),
    }
}

fn heic_bytes(container: u8, exif: Option<u8>, carrier: TransformCarrier) -> Vec<u8> {
    let options = EncodeOptions::default()
        .with_hevc_mode("intra".to_owned())
        .with_qp(0)
        .with_threads(Some(1))
        .with_grid_tile(Some(0))
        .with_transforms(transforms(container))
        .with_transform_carrier(carrier)
        .with_exif(exif.map(|value| tiff(value, ByteOrder::Little)));
    oxideav_heif::encode_rgb8(
        u32::try_from(WIDTH).expect("fixture width fits u32"),
        u32::try_from(HEIGHT).expect("fixture height fits u32"),
        &pixels(),
        &options,
    )
    .expect("encode independent HEIC fixture")
}

#[test]
fn heic_container_transforms_override_duplicate_or_conflicting_exif_for_both_carriers() {
    let work = workspace();
    let baseline_path = work.path().join("baseline.heic");
    fs::write(
        &baseline_path,
        heic_bytes(1, None, TransformCarrier::CodedItem),
    )
    .expect("write baseline HEIC");
    let baseline = codec::decode(&baseline_path, WIDTH * HEIGHT).expect("decode baseline HEIC");
    for (label, carrier) in [
        ("coded", TransformCarrier::CodedItem),
        ("identity", TransformCarrier::Identity),
    ] {
        for orientation in 2..=8 {
            for exif in [None, Some(orientation), Some(1 + orientation % 8)] {
                let source = work
                    .path()
                    .join(format!("{label}-{orientation}-{exif:?}.heic"));
                fs::write(&source, heic_bytes(orientation, exif, carrier))
                    .expect("write transformed HEIC");
                let decoded =
                    codec::decode(&source, WIDTH * HEIGHT).expect("decode transformed HEIC");
                assert_oriented(&decoded, &baseline, orientation);
                let output = source.with_extension("png");
                cli(&source, &output, &["--scale", "1"]);
                assert_normalized_png(&output, &decoded);
            }
        }
    }
}

#[test]
fn identity_wrapper_keeps_child_container_transform_authoritative() {
    let work = workspace();
    let bytes = heic_bytes(6, None, TransformCarrier::CodedItem);
    let baseline_path = work.path().join("transformed-child.heic");
    fs::write(&baseline_path, &bytes).expect("write transformed child");
    let expected = codec::decode(&baseline_path, WIDTH * HEIGHT).expect("decode transformed child");
    let file = HeifFile::parse_borrowed(&bytes).expect("parse fixture container");
    let mut writer = HeifWriter::new();
    let input = writer
        .import_item(&file, file.primary_item().expect("primary item").id)
        .expect("import transformed item");
    let primary = writer.add_identity(
        input,
        vec![(
            Property::Ispe(Ispe::new(
                u32::try_from(HEIGHT).expect("rotated fixture width fits u32"),
                u32::try_from(WIDTH).expect("rotated fixture height fits u32"),
            )),
            false,
        )],
    );
    writer.add_exif(primary, &tiff(6, ByteOrder::Little));
    writer.set_primary(primary);
    let source = work.path().join("identity-wrapper.heic");
    fs::write(
        &source,
        writer.write_to_vec().expect("encode identity wrapper"),
    )
    .expect("write identity wrapper");
    let actual = codec::decode(&source, WIDTH * HEIGHT).expect("decode identity wrapper");
    assert_eq!(
        actual, expected,
        "child transform must not be applied twice"
    );
}

#[test]
fn requested_width_uses_display_dimensions_after_rotation() {
    let work = workspace();
    let png = work.path().join("rotated.png");
    write_png(&png, Some(tiff(6, ByteOrder::Little)));
    let jpeg = work.path().join("rotated.jpg");
    write_jpeg(&jpeg, &jpeg_bytes(), 6, ByteOrder::Little);
    let heic = work.path().join("rotated.heic");
    fs::write(&heic, heic_bytes(6, None, TransformCarrier::Identity)).expect("write rotated HEIC");
    for source in [&png, &jpeg, &heic] {
        let output = work.path().join(format!(
            "{}-resized.png",
            source
                .extension()
                .expect("fixture extension")
                .to_string_lossy()
        ));
        cli(source, &output, &["--width", "24"]);
        let actual = codec::decode(&output, 24 * 36).expect("decode sized output");
        assert_eq!(
            (actual.width, actual.height),
            (24, 36),
            "width must apply to rotated display dimensions for {}",
            source.display()
        );
    }
}
