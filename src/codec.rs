//! Pure Rust PNG/JPEG/HEIC codecs with explicit color and resource policies.
//! The public boundary is top-left-origin, straight-alpha, SDR sRGB RGBA8.
use image::{DynamicImage, ImageDecoder, ImageEncoder, ImageFormat, ImageReader};
use moxcms::{ColorProfile, DataColorSpace, Layout, TransformOptions};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::fs::File;
use std::io::{BufWriter, Cursor, Read, Write};
use std::path::Path;
use std::str::FromStr;

// These caps bound codec setup independently of the numeric resizer budget.
/// Keep metadata-bearing singleton images usable without area-sized allocations.
const MIN_CODEC_ALLOCATION_BYTES: usize = 1024 * 1024;
/// Bound compressed input/item bytes relative to the permitted pixel count.
const COMPRESSED_BYTES_PER_PIXEL: usize = 8;
/// Covers an RGBA8 source plus its normalized four-channel float64 working copy.
const WORKING_BYTES_PER_PIXEL: usize = 36;
/// Conservative allowance for decoded HEVC planes and reconstruction temporaries.
const HEVC_WORKING_BYTES_PER_PIXEL: usize = 24;
/// An RGBA16 intermediate canvas needs eight bytes per output pixel.
const CANVAS_BYTES_PER_PIXEL: usize = 8;
/// Bound color/metadata decoder scratch space per permitted source pixel.
const COLOR_BYTES_PER_PIXEL: usize = 16;
/// Reject oversized parameter sets before allocating the typed HEVC syntax tree.
const MAX_PARAMETER_SET_BYTES: usize = 1024 * 1024;
/// Match the structural guard's upper bound for embedded color profiles.
const MAX_ICC_PROFILE_BYTES: usize = 16 * 1024 * 1024;
/// Bound ordinary EXIF metadata independently of the source image area.
const MAX_EXIF_BYTES: u64 = 1024 * 1024;
/// Bound repeated typed-property cloning across a derived image graph.
const MAX_ASSOCIATED_PROPERTY_BYTES: usize = 64 * 1024 * 1024;
/// Ordinary phone image grids fit comfortably within this traversal limit.
const MAX_DERIVATION_NODES: usize = 4096;
/// Cap identity/tone-map chains before recursive image-graph construction.
const MAX_DERIVATION_DEPTH: usize = 32;
/// Still images may have many slices, but may not contain unbounded NAL packets.
const MAX_IMAGE_NALS: usize = 65_536;
/// Avoid nested codec pools oversubscribing machines with many logical cores.
const MAX_HEIC_THREADS: usize = 8;

/// A decoding, encoding, resource-limit, or image-validation failure.
///
/// Foreign codec and I/O errors remain available through [`Error::source`].
#[derive(Debug)]
pub struct CodecError {
    message: String,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl CodecError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: None,
        }
    }

    fn with_source(context: &str, source: impl Error + Send + Sync + 'static) -> Self {
        Self {
            message: context.to_owned(),
            source: Some(Box::new(source)),
        }
    }
}

impl fmt::Display for CodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for CodecError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_ref().map(|source| {
            let source: &(dyn Error + 'static) = source.as_ref();
            source
        })
    }
}

impl From<&str> for CodecError {
    fn from(message: &str) -> Self {
        Self::invalid(message)
    }
}

impl From<String> for CodecError {
    fn from(message: String) -> Self {
        Self::invalid(message)
    }
}

impl From<std::num::TryFromIntError> for CodecError {
    fn from(source: std::num::TryFromIntError) -> Self {
        Self::with_source(
            "image dimension or metadata integer is not representable",
            source,
        )
    }
}

// Integer quantization is the format boundary: callers validate finite quality
// values first, and clamp final pixel values before converting their code values.
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "rounding and clamping explicitly define conversion to an 8-bit integer code"
)]
fn rounded_byte(value: f64) -> u8 {
    value.round().clamp(0.0, 255.0) as u8
}

/// File encoding selected independently of the destination path's extension.
///
/// ```
/// # use rro::codec::{CodecError, OutputFormat};
/// # fn main() -> Result<(), CodecError> {
/// let format: OutputFormat = "jpg".parse()?;
/// assert_eq!(format.to_string(), "jpeg");
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OutputFormat {
    /// Lossless PNG with RGB or straight-alpha RGBA pixels.
    Png,
    /// Lossy JPEG containing opaque RGB pixels.
    Jpeg,
    /// Lossy HEVC-coded HEIF containing opaque RGB pixels.
    Heic,
}

impl fmt::Display for OutputFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Heic => "heic",
        })
    }
}

impl FromStr for OutputFormat {
    type Err = CodecError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "png" => Ok(Self::Png),
            "jpeg" | "jpg" => Ok(Self::Jpeg),
            "heic" | "heif" => Ok(Self::Heic),
            _ => Err(CodecError::invalid(format!(
                "unsupported output format {value:?}"
            ))),
        }
    }
}

/// Whether the source image contains an alpha channel, including fully opaque alpha.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AlphaChannel {
    /// The source contains no alpha channel or transparency declaration.
    Absent,
    /// The source contains alpha samples or declares keyed/indexed transparency.
    Present,
}

/// A top-left-origin, straight-alpha, 8-bit sRGB image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaImage {
    /// Pixel columns.
    pub width: usize,
    /// Pixel rows.
    pub height: usize,
    /// Tightly packed RGBA bytes, exactly `width * height * 4` entries.
    pub pixels: Vec<u8>,
    /// Source alpha-channel presence, independent of whether its values are opaque.
    /// [`AlphaChannel::Absent`] requires all packed alpha bytes to equal 255.
    pub alpha: AlphaChannel,
}

fn check_path(path: &Path) -> Result<(), CodecError> {
    if path.as_os_str().as_encoded_bytes().contains(&0) {
        Err(CodecError::invalid("path contains an embedded NUL byte"))
    } else {
        Ok(())
    }
}
fn zeroed(length: usize) -> Result<Vec<u8>, CodecError> {
    let mut result = Vec::new();
    result.try_reserve_exact(length).map_err(|error| {
        CodecError::with_source("not enough memory for image color conversion", error)
    })?;
    result.resize(length, 0);
    Ok(result)
}
fn rgb_pixels(rgba: &[u8]) -> Result<Vec<u8>, CodecError> {
    let mut rgb = zeroed(rgba.len() / 4 * 3)?;
    for (source, target) in rgba.chunks_exact(4).zip(rgb.chunks_exact_mut(3)) {
        target.copy_from_slice(&source[..3]);
    }
    Ok(rgb)
}
fn dimensions(width: usize, height: usize, max_pixels: usize) -> Result<usize, CodecError> {
    width
        .checked_mul(height)
        .filter(|n| width > 0 && height > 0 && *n <= max_pixels)
        .and_then(|n| n.checked_mul(4))
        .filter(|n| isize::try_from(*n).is_ok())
        .ok_or_else(|| {
            CodecError::invalid("input image exceeds pixel/memory limit or has invalid dimensions")
        })
}

// Walk real JPEG markers, never mistaking an EOI-shaped byte pair inside an
// APP/EXIF payload for the end of the compressed image. Bytes after EOI are
// permitted by common JPEG containers and do not imply truncation.
fn jpeg_has_eoi(bytes: &[u8]) -> bool {
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return false;
    }
    let mut position = 2;
    let mut entropy = false;
    loop {
        if entropy {
            while position < bytes.len() && bytes[position] != 0xff {
                position += 1;
            }
        }
        if position >= bytes.len() || bytes[position] != 0xff {
            return false;
        }
        while position < bytes.len() && bytes[position] == 0xff {
            position += 1;
        }
        let Some(&marker) = bytes.get(position) else {
            return false;
        };
        position += 1;
        if entropy && (marker == 0 || (0xd0..=0xd7).contains(&marker)) {
            continue;
        }
        entropy = false;
        if marker == 0xd9 {
            return true;
        }
        if marker == 0x01 {
            continue;
        }
        if marker == 0 || marker == 0xd8 || (0xd0..=0xd7).contains(&marker) {
            return false;
        }
        let Some(length_bytes) = bytes.get(position..position.saturating_add(2)) else {
            return false;
        };
        let length = usize::from(u16::from_be_bytes([length_bytes[0], length_bytes[1]]));
        if length < 2 || length > bytes.len().saturating_sub(position) {
            return false;
        }
        position += length;
        if marker == 0xda {
            entropy = true;
        }
    }
}

enum PngColor {
    Cicp([u8; 4]),
    Icc,
    Srgb,
    Profile(Box<ColorProfile>),
    Untagged,
}

fn png_metadata(bytes: &[u8], max_pixels: usize) -> Result<(PngColor, AlphaChannel), CodecError> {
    let decoder = png::Decoder::new_with_limits(
        Cursor::new(bytes),
        png::Limits {
            bytes: max_pixels
                .saturating_mul(COLOR_BYTES_PER_PIXEL)
                .max(MIN_CODEC_ALLOCATION_BYTES),
        },
    );
    let reader = decoder
        .read_info()
        .map_err(|e| CodecError::with_source("PNG metadata decoding failed", e))?;
    let info = reader.info();
    dimensions(
        usize::try_from(info.width)?,
        usize::try_from(info.height)?,
        max_pixels,
    )?;
    let alpha = if matches!(
        info.color_type,
        png::ColorType::Rgba | png::ColorType::GrayscaleAlpha
    ) || info.trns.is_some()
    {
        AlphaChannel::Present
    } else {
        AlphaChannel::Absent
    };
    // PNG Third Edition specifies cICP > iCCP > sRGB > cHRM/gAMA.
    if let Some(cicp) = info.coding_independent_code_points {
        return Ok((
            PngColor::Cicp([
                cicp.color_primaries,
                cicp.transfer_function,
                cicp.matrix_coefficients,
                u8::from(cicp.is_video_full_range_image),
            ]),
            alpha,
        ));
    }
    if info.icc_profile.is_some() {
        return Ok((PngColor::Icc, alpha));
    }
    if info.srgb.is_some() {
        return Ok((PngColor::Srgb, alpha));
    }
    if info.gama_chunk.is_none() && info.chrm_chunk.is_none() {
        return Ok((PngColor::Untagged, alpha));
    }
    let mut profile = ColorProfile::new_srgb();
    if let Some(gamma) = info.gama_chunk {
        let exponent = 1.0 / gamma.into_value();
        if !exponent.is_finite() || !(0.1..=10.0).contains(&exponent) {
            return Err(CodecError::invalid(
                "unsupported PNG gamma (supported decoding exponents are 0.1 through 10)",
            ));
        }
        let curve = moxcms::curve_from_gamma(exponent);
        profile.red_trc = Some(curve.clone());
        profile.green_trc = Some(curve.clone());
        profile.blue_trc = Some(curve);
        profile.cicp = None;
    }
    if let Some(chrm) = info.chrm_chunk {
        let pairs = [chrm.white, chrm.red, chrm.green, chrm.blue]
            .map(|p| (p.0.into_value(), p.1.into_value()));
        if pairs.iter().any(|&(x, y)| {
            !x.is_finite() || !y.is_finite() || x < 0.0 || y <= 0.0 || x + y > 1.00001
        }) {
            return Err(CodecError::invalid("invalid PNG chromaticities"));
        }
        let [(wx, wy), (rx, ry), (gx, gy), (bx, by)] = pairs;
        let r = [rx / ry, 1.0, (1.0 - rx - ry) / ry];
        let g = [gx / gy, 1.0, (1.0 - gx - gy) / gy];
        let b = [bx / by, 1.0, (1.0 - bx - by) / by];
        let det = r[0] * (g[1] * b[2] - b[1] * g[2]) - g[0] * (r[1] * b[2] - b[1] * r[2])
            + b[0] * (r[1] * g[2] - g[1] * r[2]);
        if !det.is_finite() || det.abs() < 1e-6 {
            return Err(CodecError::invalid("degenerate PNG chromaticities"));
        }
        profile.update_rgb_colorimetry(
            moxcms::XyY::new(f64::from(wx), f64::from(wy), 1.0),
            moxcms::ColorPrimaries {
                red: moxcms::Chromaticity::new(rx, ry),
                green: moxcms::Chromaticity::new(gx, gy),
                blue: moxcms::Chromaticity::new(bx, by),
            },
        );
    }
    Ok((PngColor::Profile(Box::new(profile)), alpha))
}

fn apply_png_color(
    image: &mut RgbaImage,
    color: PngColor,
    icc: Option<Vec<u8>>,
) -> Result<(), CodecError> {
    match color {
        PngColor::Cicp([primaries, transfer, matrix, range]) => {
            if matrix != 0 || range != 1 || matches!(primaries, 0 | 2) || matches!(transfer, 0 | 2)
            {
                return Err(CodecError::invalid(
                    "unsupported PNG cICP (requires specified RGB primaries/transfer and full range)",
                ));
            }
            convert_nclx(image, primaries.into(), transfer.into())
        }
        PngColor::Icc => convert_icc(image, &icc.ok_or("PNG ICC profile could not be decoded")?),
        PngColor::Profile(profile) => convert_profile(image, &profile),
        PngColor::Srgb | PngColor::Untagged => Ok(()),
    }
}
fn convert_profile(image: &mut RgbaImage, profile: &ColorProfile) -> Result<(), CodecError> {
    if profile.cicp.is_some_and(|cicp| {
        matches!(
            cicp.transfer_characteristics,
            moxcms::TransferCharacteristics::Smpte2084 | moxcms::TransferCharacteristics::Hlg
        )
    }) {
        return Err(CodecError::invalid(
            "HDR PQ/HLG ICC profiles require tone mapping and are not supported yet",
        ));
    }
    if !matches!(
        profile.color_space,
        DataColorSpace::Rgb | DataColorSpace::Gray
    ) {
        return Err(CodecError::invalid(
            "unsupported embedded ICC color space (only RGB and grayscale profiles are supported)",
        ));
    }
    let gray;
    let (source, layout) = if profile.color_space == DataColorSpace::Gray {
        let mut buffer = zeroed(image.pixels.len() / 2)?;
        for (pixel, target) in image.pixels.chunks_exact(4).zip(buffer.chunks_exact_mut(2)) {
            target.copy_from_slice(&[pixel[0], pixel[3]]);
        }
        gray = buffer;
        (gray.as_slice(), Layout::GrayAlpha)
    } else {
        (image.pixels.as_slice(), Layout::Rgba)
    };
    let transform = profile
        .create_transform_8bit(
            layout,
            &ColorProfile::new_srgb(),
            Layout::Rgba,
            TransformOptions::default(),
        )
        .map_err(|e| CodecError::with_source("unsupported ICC color conversion", e))?;
    let mut target = zeroed(image.pixels.len())?;
    transform
        .transform(source, &mut target)
        .map_err(|e| CodecError::with_source("ICC color conversion failed", e))?;
    image.pixels = target;
    Ok(())
}
fn convert_icc(image: &mut RgbaImage, bytes: &[u8]) -> Result<(), CodecError> {
    let profile = ColorProfile::new_from_slice(bytes)
        .map_err(|e| CodecError::with_source("invalid embedded ICC profile", e))?;
    convert_profile(image, &profile)
}
fn convert_nclx(image: &mut RgbaImage, primaries: i32, transfer: i32) -> Result<(), CodecError> {
    // Missing/unspecified SDR information follows the ordinary sRGB fallback.
    if matches!(transfer, 16 | 18) {
        return Err(CodecError::invalid(
            "HDR PQ/HLG primary images require tone mapping and are not supported yet",
        ));
    }
    if matches!(primaries, 0 | 2) && matches!(transfer, 0 | 2) {
        return Ok(());
    }
    let primaries = if primaries == 2 { 1 } else { primaries };
    let transfer = if transfer == 2 { 13 } else { transfer };
    if primaries == 1 && transfer == 13 {
        return Ok(());
    }
    if !matches!(primaries, 1 | 4 | 5 | 6 | 7 | 9 | 11 | 12 | 22)
        || !matches!(transfer, 1 | 4 | 5 | 6 | 7 | 8 | 13 | 14 | 15)
    {
        return Err(CodecError::invalid(format!(
            "unsupported HEIC NCLX color primaries/transfer {primaries}/{transfer}"
        )));
    }
    let profile = moxcms::CicpProfile {
        color_primaries: moxcms::CicpColorPrimaries::try_from(u8::try_from(primaries)?)
            .map_err(|error| CodecError::with_source("color profile operation failed", error))?,
        transfer_characteristics: moxcms::TransferCharacteristics::try_from(u8::try_from(
            transfer,
        )?)
        .map_err(|error| CodecError::with_source("color profile operation failed", error))?,
        matrix_coefficients: moxcms::MatrixCoefficients::Identity,
        full_range: true,
    };
    convert_profile(image, &ColorProfile::new_from_cicp(profile))
}
#[path = "codec_limits.rs"]
mod codec_limits;

fn heic_threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get().min(MAX_HEIC_THREADS))
}

fn heic_error(error: impl Error + Send + Sync + 'static) -> CodecError {
    CodecError::with_source("heic codec", error)
}

fn heic_location_limits(
    file: &oxideav_heif::HeifFile<&[u8]>,
    max_bytes: usize,
) -> Result<(), CodecError> {
    let meta = file.meta().map_err(heic_error)?;
    for location in &meta.locations {
        if location.construction_method > 1 || location.data_reference_index != 0 {
            return Err(CodecError::invalid(
                "unsupported HEIC external or recursively constructed item data",
            ));
        }
        let source_length = if location.construction_method == 0 {
            file.bytes().len()
        } else {
            meta.idat.as_ref().map_or(0, Vec::len)
        };
        let source_length = u64::try_from(source_length)?;
        let mut total = 0u64;
        for extent in &location.extents {
            let start = location
                .base_offset
                .checked_add(extent.offset)
                .ok_or("HEIC extent offset overflow")?;
            if start > source_length {
                return Err(CodecError::invalid(
                    "HEIC item extent starts beyond its source",
                ));
            }
            let length = if extent.length == 0 {
                source_length - start
            } else {
                extent.length
            };
            if length > source_length - start {
                return Err(CodecError::invalid("HEIC item extent exceeds its source"));
            }
            total = total.checked_add(length).ok_or("HEIC item size overflow")?;
            if total > u64::try_from(max_bytes)? {
                return Err(CodecError::invalid(
                    "HEIC item data exceeds compressed-byte/memory limit",
                ));
            }
        }
    }
    Ok(())
}

/// Discard unsupported auxiliary renditions in the parsed view, leaving the
/// input bytes immutable. The library otherwise decodes depth and gain maps
/// even when its `tone_mapped` option is false.
fn heic_primary_view(file: &mut oxideav_heif::HeifFile<&[u8]>) -> Result<u32, CodecError> {
    use oxideav_heif::{AuxKind, ItemProperties};
    let meta = file
        .meta
        .as_mut()
        .ok_or("HEIC has no still-image item metadata")?;
    let mut primary = meta
        .primary_item_id
        .ok_or("HEIC has no primary still image")?;
    for _ in 0..MAX_DERIVATION_DEPTH {
        let item = meta.item(primary).ok_or("HEIC primary item is missing")?;
        if item.item_type != *b"tmap" {
            break;
        }
        primary = *meta
            .derivation_inputs(primary)
            .first()
            .ok_or("HEIC tone-map primary has no SDR base")?;
    }
    if meta
        .item(primary)
        .is_some_and(|item| item.item_type == *b"tmap")
    {
        return Err(CodecError::invalid(
            "HEIC tone-map chain exceeds depth limit",
        ));
    }
    let removed: HashSet<u32> = meta
        .items
        .iter()
        .filter(|item| item.item_type == *b"tmap")
        .map(|item| item.id)
        .collect();
    let ignored_aux: HashSet<u32> = meta
        .items
        .iter()
        .filter_map(|item| {
            ItemProperties::resolve(meta, item.id)
                .ok()
                .and_then(|properties| {
                    properties
                        .auxc()
                        .filter(|aux| aux.kind() != AuxKind::Alpha)
                        .map(|_| item.id)
                })
        })
        .collect();
    let ignored_metadata: HashSet<u32> = meta
        .items
        .iter()
        .filter(|item| item.is_xmp())
        .map(|item| item.id)
        .collect();
    meta.primary_item_id = Some(primary);
    meta.items.retain(|item| !removed.contains(&item.id));
    meta.references.retain(|reference| {
        if removed.contains(&reference.from_item_id)
            || reference.to_item_ids.iter().any(|id| removed.contains(id))
            || (reference.reference_type == *b"auxl"
                && ignored_aux.contains(&reference.from_item_id))
        {
            return false;
        }
        !(reference.reference_type == *b"cdsc"
            && ignored_metadata.contains(&reference.from_item_id))
    });
    Ok(primary)
}

fn heic_graph_budget(
    file: &oxideav_heif::HeifFile<&[u8]>,
    primary: u32,
    budget: usize,
) -> Result<(), CodecError> {
    fn visit(
        meta: &oxideav_heif::Meta,
        item: u32,
        depth: usize,
        ancestors: &mut HashSet<u32>,
        count: &mut usize,
        property_bytes: &mut usize,
        budget: usize,
    ) -> Result<(), CodecError> {
        *count += 1;
        if *count > MAX_DERIVATION_NODES || depth > MAX_DERIVATION_DEPTH {
            return Err(CodecError::invalid(
                "HEIC derivation graph exceeds resource limits",
            ));
        }
        if !ancestors.insert(item) {
            return Err(CodecError::invalid("cyclic HEIC derivation graph"));
        }
        for association in meta.associations.iter().filter(|a| a.item_id == item) {
            for entry in &association.entries {
                if entry.index == 0 {
                    continue;
                }
                let property = meta
                    .properties
                    .get(usize::from(entry.index) - 1)
                    .ok_or("HEIC property index is invalid")?;
                *property_bytes = property_bytes
                    .checked_add(property.body.len())
                    .ok_or("HEIC property memory overflow")?;
                if *property_bytes > budget {
                    return Err(CodecError::invalid(
                        "HEIC repeated properties exceed metadata/memory limit",
                    ));
                }
            }
        }
        for reference in &meta.references {
            if reference.reference_type == *b"dimg" && reference.from_item_id == item {
                for &child in &reference.to_item_ids {
                    visit(
                        meta,
                        child,
                        depth + 1,
                        ancestors,
                        count,
                        property_bytes,
                        budget,
                    )?;
                }
            } else if matches!(&reference.reference_type, b"auxl" | b"thmb")
                && reference.to_item_ids.contains(&item)
            {
                visit(
                    meta,
                    reference.from_item_id,
                    depth + 1,
                    ancestors,
                    count,
                    property_bytes,
                    budget,
                )?;
            }
        }
        ancestors.remove(&item);
        Ok(())
    }
    visit(
        file.meta().map_err(heic_error)?,
        primary,
        0,
        &mut HashSet::new(),
        &mut 0,
        &mut 0,
        budget,
    )
}

fn heic_sps_area(
    nal: &[u8],
    memory_limit: usize,
) -> Result<Option<(usize, oxideav_heif::ColorInfo)>, CodecError> {
    if nal.len() < 2 {
        return Err(CodecError::invalid(
            "HEIC contains a truncated HEVC NAL unit",
        ));
    }
    let header = oxideav_h265::nal::NalHeader::parse(nal).map_err(heic_error)?;
    if header.nuh_layer_id != 0 {
        return Err(CodecError::invalid(
            "layered HEVC images are not supported yet",
        ));
    }
    if header.nal_unit_type != 33 {
        return Ok(None);
    }
    if nal.len() > MAX_PARAMETER_SET_BYTES {
        return Err(CodecError::invalid(
            "HEIC sequence parameter set exceeds metadata limit",
        ));
    }
    let rbsp = oxideav_h265::nal::strip_emulation_prevention(&nal[2..]);
    let sps = oxideav_h265::sps::SeqParameterSet::parse(&rbsp).map_err(heic_error)?;
    let area = (usize::try_from(sps.pic_width_in_luma_samples)?)
        .checked_mul(usize::try_from(sps.pic_height_in_luma_samples)?)
        .ok_or("HEVC source dimensions overflow")?;
    if area == 0
        || area
            .checked_mul(HEVC_WORKING_BYTES_PER_PIXEL)
            .is_none_or(|bytes| bytes > memory_limit)
    {
        return Err(CodecError::invalid(
            "HEVC coded source exceeds pixel/memory limit before decoding",
        ));
    }
    if sps
        .vui_parameters
        .as_ref()
        .and_then(|vui| vui.video_signal_type.as_ref())
        .and_then(|signal| signal.colour_description.as_ref())
        .is_some_and(|color| matches!(color.transfer_characteristics, 16 | 18))
    {
        return Err(CodecError::invalid(
            "HDR PQ/HLG primary images require tone mapping and are not supported yet",
        ));
    }
    // An absent container NCLX must not replace HEVC's legal-range signal
    // with the container API's full-range MIAF fallback. Carry VUI to the RGB
    // conversion before any clipping or ICC conversion takes place.
    let signal = sps
        .vui_parameters
        .as_ref()
        .and_then(|vui| vui.video_signal_type.as_ref());
    let description = signal.and_then(|signal| signal.colour_description);
    let default_code = |value: u8, fallback: u8| if value == 2 { fallback } else { value };
    let color = oxideav_heif::ColorInfo::new(
        if signal.is_some_and(|signal| signal.video_full_range_flag) {
            oxideav_heif::ColorRange::Full
        } else {
            oxideav_heif::ColorRange::Limited
        },
        description.map_or(1, |color| default_code(color.colour_primaries, 1)),
        description.map_or(13, |color| default_code(color.transfer_characteristics, 13)),
        description.map_or(6, |color| default_code(color.matrix_coeffs, 6)),
    );
    Ok(Some((area, color)))
}

fn heic_node_limits(
    file: &oxideav_heif::HeifFile<&[u8]>,
    node: &oxideav_heif::ImageNode,
    memory_limit: usize,
    seen: &mut HashMap<u32, (usize, oxideav_heif::ColorInfo)>,
    canvas_bytes: &mut usize,
    coded_bytes: &mut usize,
) -> Result<(), CodecError> {
    use oxideav_heif::ImageKind;
    if matches!(
        node.kind,
        ImageKind::Tiled(_) | ImageKind::ColourFormatEnhancement(_)
    ) {
        return Err(CodecError::invalid(
            "HEIC tile-item and color-enhancement derivations are not supported yet",
        ));
    }
    // Unlike crop/rotation/mirror, iscl may expand an intermediate image
    // before a later crop restores a small output. It is outside v1 scope.
    if node
        .properties
        .transformative()
        .any(|entry| matches!(entry.property, oxideav_heif::Property::Iscl(_)))
    {
        return Err(CodecError::invalid(
            "HEIC scaling transforms (iscl) are not supported yet",
        ));
    }
    if let Some((width, height)) = node.ispe()
        && (usize::try_from(width)?)
            .checked_mul(usize::try_from(height)?)
            .and_then(|n| n.checked_mul(CANVAS_BYTES_PER_PIXEL))
            .is_none_or(|bytes| bytes > memory_limit)
    {
        return Err(CodecError::invalid(
            "HEIC intermediate canvas exceeds memory limit",
        ));
    }
    // The compositor allocates descriptor dimensions, not ispe or the final
    // cropped dimensions. Nested grids and overlay inputs may coexist, so use
    // a conservative sum over every derivation occurrence, including repeats.
    let canvas = match &node.kind {
        ImageKind::Grid(grid) => Some((grid.output_width, grid.output_height)),
        ImageKind::Overlay(overlay) => Some((overlay.output_width, overlay.output_height)),
        _ => None,
    };
    if let Some((width, height)) = canvas {
        let bytes = usize::try_from(width)?
            .checked_mul(usize::try_from(height)?)
            .filter(|area| *area > 0)
            .and_then(|area| area.checked_mul(CANVAS_BYTES_PER_PIXEL))
            .ok_or("HEIC derivation canvas dimensions are invalid or overflow")?;
        *canvas_bytes = canvas_bytes
            .checked_add(bytes)
            .filter(|total| *total <= memory_limit)
            .ok_or("HEIC derivation canvases exceed aggregate memory limit")?;
    }
    if node
        .properties
        .icc_profile()
        .is_some_and(|profile| profile.len() > MAX_ICC_PROFILE_BYTES)
    {
        return Err(CodecError::invalid(
            "HEIC ICC profile exceeds metadata limit",
        ));
    }
    for metadata in &node.metadata {
        if metadata.item_type == *b"dExf" {
            return Err(CodecError::invalid(
                "compressed HEIC EXIF metadata is not supported yet",
            ));
        }
        let location = file
            .meta()
            .map_err(heic_error)?
            .location(metadata.id)
            .ok_or("HEIC metadata location is missing")?;
        if location.declared_size() > MAX_EXIF_BYTES
            || location.extents.iter().any(|extent| extent.length == 0)
        {
            return Err(CodecError::invalid(
                "HEIC EXIF metadata exceeds limit or has an unbounded extent",
            ));
        }
    }
    heic_coded_limits(file, node, memory_limit, seen)?;
    if matches!(node.kind, ImageKind::Coded(_)) {
        let (area, _) = seen
            .get(&node.item.id)
            .ok_or("HEIC coded source limits are missing")?;
        // Overlay inputs coexist, and the dependency clones cached frames on
        // repeated uses. Count occurrences even when SPS parsing is deduplicated.
        *coded_bytes = area
            .checked_mul(HEVC_WORKING_BYTES_PER_PIXEL)
            .and_then(|bytes| coded_bytes.checked_add(bytes))
            .filter(|total| *total <= memory_limit)
            .ok_or("HEIC coded reconstructions exceed aggregate memory limit")?;
    }
    for input in &node.inputs {
        heic_node_limits(file, input, memory_limit, seen, canvas_bytes, coded_bytes)?;
    }
    if let Some(alpha) = &node.alpha {
        heic_node_limits(file, alpha, memory_limit, seen, canvas_bytes, coded_bytes)?;
    }
    Ok(())
}

fn heic_coded_limits(
    file: &oxideav_heif::HeifFile<&[u8]>,
    node: &oxideav_heif::ImageNode,
    memory_limit: usize,
    seen: &mut HashMap<u32, (usize, oxideav_heif::ColorInfo)>,
) -> Result<(), CodecError> {
    use oxideav_heif::ImageKind;
    if let ImageKind::Coded(kind) = &node.kind {
        if !matches!(kind, b"hvc1" | b"hev1") {
            return Err(CodecError::invalid(
                "unsupported HEIF coded image (only HEVC/HEIC is enabled)",
            ));
        }
        if let std::collections::hash_map::Entry::Vacant(entry) = seen.entry(node.item.id) {
            let configuration = node
                .properties
                .hvcc()
                .ok_or("HEIC item has no HEVC configuration")?;
            let mut largest_area = 0;
            let mut source_color = oxideav_heif::ColorInfo::default();
            for nal in configuration
                .arrays
                .iter()
                .flat_map(|array| &array.nal_units)
            {
                if nal.first().is_some_and(|byte| (byte >> 1) & 63 <= 31) {
                    return Err(CodecError::invalid(
                        "HEIC decoder configuration contains coded pictures",
                    ));
                }
                if let Some((area, color)) = heic_sps_area(nal, memory_limit)? {
                    largest_area = largest_area.max(area);
                    source_color = color;
                }
            }
            let data = file.item_data(node.item.id).map_err(heic_error)?;
            let mut position = 0usize;
            let mut first_slices = 0;
            let mut nal_count = 0;
            while position < data.len() {
                let prefix = usize::from(configuration.length_size);
                if !(1..=4).contains(&prefix) || prefix > data.len() - position {
                    return Err(CodecError::invalid("invalid HEIC NAL length prefix"));
                }
                let mut length = 0usize;
                for byte in &data[position..position + prefix] {
                    length = (length << 8) | usize::from(*byte);
                }
                position += prefix;
                if length < 2 || length > data.len() - position {
                    return Err(CodecError::invalid("truncated HEIC NAL payload"));
                }
                let nal = &data[position..position + length];
                if let Some((area, color)) = heic_sps_area(nal, memory_limit)? {
                    largest_area = largest_area.max(area);
                    source_color = color;
                }
                if (nal[0] >> 1) & 63 <= 31 && nal.len() > 2 && nal[2] & 0x80 != 0 {
                    first_slices += 1;
                }
                nal_count += 1;
                if first_slices > 1 || nal_count > MAX_IMAGE_NALS {
                    return Err(CodecError::invalid(
                        "HEIC item contains a video sequence or exceeds NAL count limits",
                    ));
                }
                position += length;
            }
            if largest_area == 0 {
                return Err(CodecError::invalid(
                    "HEIC item contains no valid sequence parameter set",
                ));
            }
            entry.insert((largest_area, source_color));
            let total_area = seen
                .values()
                .try_fold(0usize, |sum, (area, _)| sum.checked_add(*area))
                .ok_or("HEIC source area overflow")?;
            if total_area
                .checked_mul(HEVC_WORKING_BYTES_PER_PIXEL)
                .is_none_or(|bytes| bytes > memory_limit)
            {
                return Err(CodecError::invalid(
                    "HEIC coded items exceed aggregate memory limit",
                ));
            }
        }
    }
    Ok(())
}

fn rgba8_image(image: DynamicImage, alpha: AlphaChannel) -> Result<RgbaImage, CodecError> {
    let rgba = image.into_rgba8();
    Ok(RgbaImage {
        width: usize::try_from(rgba.width())?,
        height: usize::try_from(rgba.height())?,
        pixels: rgba.into_raw(),
        alpha,
    })
}

fn apply_orientation(
    image: RgbaImage,
    orientation: image::metadata::Orientation,
) -> Result<RgbaImage, CodecError> {
    let rgba = image::RgbaImage::from_raw(
        u32::try_from(image.width)?,
        u32::try_from(image.height)?,
        image.pixels,
    )
    .ok_or("invalid orientation dimensions")?;
    let mut oriented = DynamicImage::ImageRgba8(rgba);
    oriented.apply_orientation(orientation);
    rgba8_image(oriented, image.alpha)
}

fn primary_container_oriented(node: &oxideav_heif::ImageNode) -> bool {
    node.properties.irot().is_some()
        || node.properties.imir().is_some()
        || (matches!(node.kind, oxideav_heif::ImageKind::Identity)
            && node.inputs.first().is_some_and(primary_container_oriented))
}

fn heic_property_budget(
    file: &oxideav_heif::HeifFile<&[u8]>,
    memory_limit: usize,
) -> Result<usize, CodecError> {
    // Bound property cloning before resolving every auxiliary's typed properties.
    let meta = file.meta().map_err(heic_error)?;
    let metadata_budget = memory_limit.min(MAX_ASSOCIATED_PROPERTY_BYTES);
    let mut associated_bytes = 0usize;
    for association in &meta.associations {
        for entry in &association.entries {
            if entry.index != 0 {
                let property = meta
                    .properties
                    .get(usize::from(entry.index) - 1)
                    .ok_or("invalid HEIC property association")?;
                associated_bytes = associated_bytes
                    .checked_add(property.body.len())
                    .ok_or("HEIC property size overflow")?;
                if associated_bytes > metadata_budget {
                    return Err(CodecError::invalid(
                        "HEIC associated properties exceed metadata/memory limit",
                    ));
                }
            }
        }
    }
    Ok(metadata_budget)
}

fn heic_decode_limits(
    file: &oxideav_heif::HeifFile<&[u8]>,
    node: &oxideav_heif::ImageNode,
    memory_limit: usize,
) -> Result<HashMap<u32, (usize, oxideav_heif::ColorInfo)>, CodecError> {
    let mut coded_sources = HashMap::new();
    let mut canvas_bytes = 0usize;
    let mut coded_bytes = 0usize;
    heic_node_limits(
        file,
        node,
        memory_limit,
        &mut coded_sources,
        &mut canvas_bytes,
        &mut coded_bytes,
    )?;
    if coded_bytes
        .checked_add(canvas_bytes)
        .is_none_or(|bytes| bytes > memory_limit)
    {
        return Err(CodecError::invalid(
            "HEIC coded items and derivation canvases exceed aggregate memory limit",
        ));
    }
    Ok(coded_sources)
}

fn unpremultiply_rgba(pixels: &mut [u8]) -> Result<(), CodecError> {
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = u32::from(pixel[3]);
        for color in &mut pixel[..3] {
            *color = if alpha == 0 {
                0
            } else {
                u8::try_from(((u32::from(*color) * 255 + alpha / 2) / alpha).min(255))?
            };
        }
    }
    Ok(())
}

fn decode_heic(bytes: &[u8], max_pixels: usize) -> Result<RgbaImage, CodecError> {
    codec_limits::preflight(bytes)?;
    if !oxideav_heif::probe(bytes) {
        return Err(CodecError::invalid("unsupported HEIC/HEIF file brand"));
    }
    let mut file = oxideav_heif::HeifFile::parse_borrowed(bytes).map_err(heic_error)?;
    let byte_limit = max_pixels
        .saturating_mul(COMPRESSED_BYTES_PER_PIXEL)
        .max(MIN_CODEC_ALLOCATION_BYTES);
    let memory_limit = max_pixels
        .saturating_mul(WORKING_BYTES_PER_PIXEL)
        .max(MIN_CODEC_ALLOCATION_BYTES);
    heic_location_limits(&file, byte_limit)?;
    let metadata_budget = heic_property_budget(&file, memory_limit)?;
    let primary = heic_primary_view(&mut file)?;
    heic_graph_budget(&file, primary, metadata_budget)?;
    let node = oxideav_heif::build_primary_graph(&file).map_err(heic_error)?;
    let (width, height) = node.output_size().map_err(heic_error)?;
    dimensions(
        usize::try_from(width)?,
        usize::try_from(height)?,
        max_pixels,
    )?;
    let coded_sources = heic_decode_limits(&file, &node, memory_limit)?;
    let source_color = if node.properties.nclx().is_none() {
        let mut color = None;
        for coded in node.coded_items() {
            let signal = coded
                .properties
                .nclx()
                .map(oxideav_heif::ColorInfo::from_colr)
                .or_else(|| coded_sources.get(&coded.item.id).map(|(_, color)| *color))
                .ok_or("HEIC coded color signal is missing")?;
            if color.is_some_and(|color| color != signal) {
                return Err(CodecError::invalid(
                    "HEIC derived inputs use conflicting color signals",
                ));
            }
            color = Some(signal);
        }
        color
    } else {
        None
    };
    let container_oriented = primary_container_oriented(&node);
    let options = oxideav_heif::DecodeOptions::default()
        .with_max_pixels(Some(u64::try_from(max_pixels)?))
        .with_max_bytes(Some(u64::try_from(byte_limit)?))
        .with_item_id(Some(primary))
        .with_threads(Some(heic_threads()));
    let decoded = oxideav_heif::decode_file_item(&file, &options).map_err(heic_error)?;
    let premultiplied = decoded.premultiplied_alpha;
    let alpha = if decoded.frame.format.has_alpha {
        AlphaChannel::Present
    } else {
        AlphaChannel::Absent
    };
    let mut image: oxideav_heif::HeifImage = decoded.into();
    if let Some(color) = source_color {
        image.color = color;
    }
    let (primaries, transfer) = (
        i32::from(image.color.primaries),
        i32::from(image.color.transfer),
    );
    if matches!(transfer, 16 | 18) {
        return Err(CodecError::invalid(
            "HDR PQ/HLG primary images require tone mapping and are not supported yet",
        ));
    }
    dimensions(
        usize::try_from(image.width)?,
        usize::try_from(image.height)?,
        max_pixels,
    )?;
    let mut result = RgbaImage {
        width: usize::try_from(image.width)?,
        height: usize::try_from(image.height)?,
        pixels: image.try_to_rgba8().map_err(heic_error)?,
        alpha,
    };
    if premultiplied {
        unpremultiply_rgba(&mut result.pixels)?;
    }
    if let Some(profile) = &image.metadata.icc {
        convert_icc(&mut result, profile)?;
    } else {
        convert_nclx(&mut result, primaries, transfer)?;
    }
    if !container_oriented
        && let Some(exif) = &image.metadata.exif
        && let Some(orientation) = image::metadata::Orientation::from_exif_chunk(exif)
    {
        result = apply_orientation(result, orientation)?;
    }
    Ok(result)
}

fn encode_heic(
    image: &RgbaImage,
    profile: Vec<u8>,
    quality: f64,
    exif: Option<Vec<u8>>,
    transforms: Vec<oxideav_heif::Property>,
) -> Result<Vec<u8>, CodecError> {
    // Explicit lossy intra mode; PCM bootstrap is never a silent fallback.
    let mut options = oxideav_heif::EncodeOptions::default()
        .with_hevc_mode("intra".to_owned())
        .with_qp(rounded_byte((1.0 - quality) * 51.0))
        .with_threads(Some(heic_threads()));
    options.icc_profile = Some(profile);
    options.exif = exif;
    options.transforms = transforms;
    let rgb = rgb_pixels(&image.pixels)?;
    oxideav_heif::encode_rgb8(
        u32::try_from(image.width)?,
        u32::try_from(image.height)?,
        &rgb,
        &options,
    )
    .map_err(heic_error)
}

/// Decode supported content with a file-size bound before reading and a pixel
/// bound before decoder pixel allocation. Lossy codec working memory remains a
/// codec estimate; dimensions and overflow are checked independently.
/// Source alpha-channel presence is retained independently of sample opacity.
///
/// # Errors
/// Returns an error for unsupported/corrupt content, unsupported HDR or color
/// profiles, exceeded byte/pixel/resource limits, allocation failures, or file I/O.
/// The input file is never modified.
pub fn decode(path: &Path, max_pixels: usize) -> Result<RgbaImage, CodecError> {
    check_path(path)?;
    if max_pixels == 0 {
        return Err(CodecError::invalid("image pixel limit is zero"));
    }
    let mut file = File::open(path).map_err(|e| CodecError::with_source("cannot open input", e))?;
    let max_bytes = max_pixels
        .saturating_mul(COMPRESSED_BYTES_PER_PIXEL)
        .max(MIN_CODEC_ALLOCATION_BYTES)
        .min(isize::MAX.unsigned_abs() - 1);
    let file_size = file
        .metadata()
        .map_err(|e| CodecError::with_source("cannot inspect input", e))?
        .len();
    if file_size > u64::try_from(max_bytes)? {
        return Err(CodecError::invalid(
            "input file exceeds compressed-byte/memory limit",
        ));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(usize::try_from(file_size)?)
        .map_err(|error| CodecError::with_source("not enough memory to read input image", error))?;
    (&mut file)
        .take(u64::try_from(max_bytes)? + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| CodecError::with_source("cannot read input", e))?;
    if bytes.len() > max_bytes {
        return Err(CodecError::invalid(
            "input file exceeds compressed-byte/memory limit",
        ));
    }
    let format = image::guess_format(&bytes).ok();
    let mut result = match format {
        Some(format @ (ImageFormat::Png | ImageFormat::Jpeg)) => {
            if format == ImageFormat::Jpeg && !jpeg_has_eoi(&bytes) {
                return Err(CodecError::invalid(
                    "JPEG input is truncated or has no structural end-of-image marker",
                ));
            }
            let png_metadata = if format == ImageFormat::Png {
                Some(png_metadata(&bytes, max_pixels)?)
            } else {
                None
            };
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(u32::try_from(max_pixels).unwrap_or(u32::MAX));
            limits.max_image_height = limits.max_image_width;
            // Even a 1-pixel PNG can carry an ICC profile and zlib metadata
            // state. Keep a bounded metadata allowance independent of area.
            limits.max_alloc = Some(u64::try_from(
                max_pixels
                    .saturating_mul(COLOR_BYTES_PER_PIXEL)
                    .max(MIN_CODEC_ALLOCATION_BYTES),
            )?);
            let mut reader = ImageReader::with_format(Cursor::new(&bytes), format);
            reader.limits(limits);
            let mut decoder = reader
                .into_decoder()
                .map_err(|e| CodecError::with_source("image header decoding failed", e))?;
            let (width, height) = decoder.dimensions();
            dimensions(
                usize::try_from(width)?,
                usize::try_from(height)?,
                max_pixels,
            )?;
            let orientation = decoder
                .orientation()
                .map_err(|e| CodecError::with_source("invalid image orientation", e))?;
            let icc = decoder
                .icc_profile()
                .map_err(|e| CodecError::with_source("invalid ICC metadata", e))?;
            let mut image = DynamicImage::from_decoder(decoder)
                .map_err(|e| CodecError::with_source("image decoding failed", e))?;
            image.apply_orientation(orientation);
            let mut result = rgba8_image(
                image,
                png_metadata
                    .as_ref()
                    .map_or(AlphaChannel::Absent, |(_, alpha)| *alpha),
            )?;
            if let Some((color, _)) = png_metadata {
                apply_png_color(&mut result, color, icc)?;
            } else if let Some(profile) = icc {
                convert_icc(&mut result, &profile)?;
            }
            result
        }
        _ if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" => decode_heic(&bytes, max_pixels)?,
        _ => {
            return Err(CodecError::invalid(
                "unsupported image content (expected PNG, JPEG, or HEIC/HEIF)",
            ));
        }
    };
    for pixel in result.pixels.chunks_exact_mut(4) {
        if pixel[3] == 0 {
            pixel[..3].fill(0);
        }
    }
    Ok(result)
}

/// Encode straight sRGB RGBA8. JPEG/HEIC require opaque input; the product
/// pipeline applies white compositing. Caller owns atomic publication policy.
/// PNG writes RGB for [`AlphaChannel::Absent`] and RGBA for [`AlphaChannel::Present`].
///
/// # Errors
/// Returns an error for invalid image dimensions, contradictory alpha metadata, non-opaque lossy input,
/// non-finite/out-of-range quality, allocation/codec failures, or file I/O.
/// The destination may contain partial bytes on error: callers requiring atomic
/// publication should encode to a temporary file and publish it after success.
pub fn encode(
    image: &RgbaImage,
    path: &Path,
    format: OutputFormat,
    quality: f64,
) -> Result<(), CodecError> {
    check_path(path)?;
    let length = image
        .width
        .checked_mul(image.height)
        .and_then(|n| n.checked_mul(4));
    if image.width == 0
        || image.height == 0
        || length != Some(image.pixels.len())
        || u32::try_from(image.width).is_err()
        || u32::try_from(image.height).is_err()
    {
        return Err(CodecError::invalid(
            "invalid RGBA image dimensions or pixel buffer length",
        ));
    }
    if !quality.is_finite() || !(0.0..=1.0).contains(&quality) {
        return Err(CodecError::invalid("quality must be between zero and one"));
    }
    if image.alpha == AlphaChannel::Absent && image.pixels.chunks_exact(4).any(|p| p[3] != 255) {
        return Err(CodecError::invalid(
            "image declares no alpha channel but contains nonopaque pixels",
        ));
    }
    if format != OutputFormat::Png && image.pixels.chunks_exact(4).any(|p| p[3] != 255) {
        return Err(CodecError::invalid(
            "JPEG/HEIC output requires opaque pixels; composite alpha before encoding",
        ));
    }
    let profile = ColorProfile::new_srgb()
        .encode()
        .map_err(|e| CodecError::with_source("cannot create sRGB output profile", e))?;
    let file = File::create(path).map_err(|e| CodecError::with_source("cannot open output", e))?;
    let mut writer = BufWriter::new(file);
    let (w, h) = (u32::try_from(image.width)?, u32::try_from(image.height)?);
    match format {
        OutputFormat::Png => {
            let rgb;
            let (pixels, color) = match image.alpha {
                AlphaChannel::Absent => {
                    rgb = rgb_pixels(&image.pixels)?;
                    (rgb.as_slice(), image::ExtendedColorType::Rgb8)
                }
                AlphaChannel::Present => (image.pixels.as_slice(), image::ExtendedColorType::Rgba8),
            };
            let mut encoder = image::codecs::png::PngEncoder::new(&mut writer);
            encoder.set_icc_profile(profile).map_err(|error| {
                CodecError::with_source("color profile operation failed", error)
            })?;
            encoder
                .write_image(pixels, w, h, color)
                .map_err(|e| CodecError::with_source("PNG encoding failed", e))?;
        }
        OutputFormat::Jpeg => {
            let rgb = rgb_pixels(&image.pixels)?;
            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(
                &mut writer,
                rounded_byte(quality * 100.0),
            );
            encoder.set_icc_profile(profile).map_err(|error| {
                CodecError::with_source("color profile operation failed", error)
            })?;
            encoder
                .encode(&rgb, w, h, image::ExtendedColorType::Rgb8)
                .map_err(|e| CodecError::with_source("JPEG encoding failed", e))?;
        }
        OutputFormat::Heic => {
            let output = encode_heic(image, profile, quality, None, Vec::new())?;
            writer
                .write_all(&output)
                .map_err(|error| CodecError::with_source("cannot write HEIC output", error))?;
        }
    }
    writer
        .flush()
        .map_err(|e| CodecError::with_source("cannot finalize output", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct TestDir(std::path::PathBuf);
    impl TestDir {
        fn new() -> Self {
            let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("reports/work")
                .join(format!(
                    "codec-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
            std::fs::create_dir_all(&directory).unwrap();
            Self(directory)
        }
        fn path(&self, name: &str) -> std::path::PathBuf {
            self.0.join(name)
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn sample() -> RgbaImage {
        let mut pixels = Vec::new();
        for y in 0u8..48 {
            for x in 0u8..64 {
                pixels.extend_from_slice(&[32 + x * 2, 48 + y * 2, 64 + x + y, 255]);
            }
        }
        RgbaImage {
            width: 64,
            height: 48,
            pixels,
            alpha: AlphaChannel::Absent,
        }
    }

    #[test]
    fn png_opaque_is_lossless_and_unicode_paths_work() {
        let dir = TestDir::new();
        let path = dir.path("opaque café 图像.png");
        let image = sample();
        encode(&image, &path, OutputFormat::Png, 0.9).unwrap();
        assert_eq!(decode(&path, 64 * 48).unwrap(), image);
        assert!(
            decode(&path, 64 * 48 - 1)
                .unwrap_err()
                .to_string()
                .contains("limit")
        );
    }

    #[test]
    fn png_alpha_uses_straight_rgba_without_small_alpha_quantization() {
        let dir = TestDir::new();
        let path = dir.path("alpha.png");
        let mut image = RgbaImage {
            width: 256,
            height: 1,
            pixels: Vec::new(),
            alpha: AlphaChannel::Present,
        };
        for alpha in 0..=255 {
            image.pixels.extend_from_slice(&[17, 129, 237, alpha]);
        }
        encode(&image, &path, OutputFormat::Png, 0.9).unwrap();
        let decoded = decode(&path, 256).unwrap();
        assert_eq!(decoded.alpha, AlphaChannel::Present);
        assert_eq!(&decoded.pixels[..4], &[0, 0, 0, 0]);
        assert_eq!(&decoded.pixels[4..], &image.pixels[4..]);
    }

    #[test]
    fn png_channel_presence_survives_decode_and_reencode() {
        let dir = TestDir::new();
        for (name, color, input, transparency, alpha) in [
            (
                "rgb",
                png::ColorType::Rgb,
                vec![17, 23, 41],
                None,
                AlphaChannel::Absent,
            ),
            (
                "rgba-opaque",
                png::ColorType::Rgba,
                vec![17, 23, 41, 255],
                None,
                AlphaChannel::Present,
            ),
            (
                "gray",
                png::ColorType::Grayscale,
                vec![17],
                None,
                AlphaChannel::Absent,
            ),
            (
                "gray-alpha-opaque",
                png::ColorType::GrayscaleAlpha,
                vec![17, 255],
                None,
                AlphaChannel::Present,
            ),
            (
                "indexed",
                png::ColorType::Indexed,
                vec![0],
                None,
                AlphaChannel::Absent,
            ),
            (
                "indexed-opaque-alpha",
                png::ColorType::Indexed,
                vec![0],
                Some(vec![255]),
                AlphaChannel::Present,
            ),
            (
                "indexed-transparent",
                png::ColorType::Indexed,
                vec![0],
                Some(vec![0]),
                AlphaChannel::Present,
            ),
            (
                "gray-keyed",
                png::ColorType::Grayscale,
                vec![17],
                Some(vec![0, 99]),
                AlphaChannel::Present,
            ),
            (
                "rgb-keyed",
                png::ColorType::Rgb,
                vec![17, 23, 41],
                Some(vec![0, 99, 0, 99, 0, 99]),
                AlphaChannel::Present,
            ),
        ] {
            let source = dir.path(&format!("{name}.png"));
            let mut encoder = png::Encoder::new(File::create(&source).unwrap(), 1, 1);
            encoder.set_color(color);
            encoder.set_depth(png::BitDepth::Eight);
            if color == png::ColorType::Indexed {
                encoder.set_palette(vec![17, 23, 41]);
            }
            if let Some(transparency) = transparency {
                encoder.set_trns(transparency);
            }
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&input)
                .unwrap();
            let decoded = decode(&source, 1).unwrap();
            assert_eq!(decoded.alpha, alpha, "source {name}");
            let output = dir.path(&format!("{name}-output.png"));
            encode(&decoded, &output, OutputFormat::Png, 0.9).unwrap();
            let reader = png::Decoder::new(Cursor::new(std::fs::read(&output).unwrap()))
                .read_info()
                .unwrap();
            let expected_color = match alpha {
                AlphaChannel::Absent => png::ColorType::Rgb,
                AlphaChannel::Present => png::ColorType::Rgba,
            };
            assert_eq!(reader.info().color_type, expected_color, "encoded {name}");
            assert_eq!(decode(&output, 1).unwrap(), decoded, "round trip {name}");
        }
    }

    #[test]
    fn missing_alpha_metadata_rejects_transparency_before_opening_output() {
        let dir = TestDir::new();
        let mut image = sample();
        image.pixels[3] = 128;
        let path = dir.path("preserved.png");
        std::fs::write(&path, b"preserved destination").unwrap();
        let error = encode(&image, &path, OutputFormat::Png, 0.9).unwrap_err();
        assert!(error.to_string().contains("no alpha channel"));
        assert_eq!(std::fs::read(path).unwrap(), b"preserved destination");
    }

    #[test]
    fn heic_opaque_alpha_auxiliary_is_preserved_as_present() {
        let dir = TestDir::new();
        let path = dir.path("opaque-alpha.heic");
        let pixels = [80, 160, 220, 255].repeat(8 * 6);
        let options = oxideav_heif::EncodeOptions::default()
            .with_hevc_mode("intra".to_owned())
            .with_qp(0)
            .with_threads(Some(1));
        let encoded = oxideav_heif::encode_rgba8(8, 6, &pixels, &options).unwrap();
        std::fs::write(&path, encoded).unwrap();
        let decoded = decode(&path, 8 * 6).unwrap();
        assert_eq!(decoded.alpha, AlphaChannel::Present);
        assert!(decoded.pixels.chunks_exact(4).all(|pixel| pixel[3] == 255));
        let output = dir.path("opaque-alpha.png");
        encode(&decoded, &output, OutputFormat::Png, 0.9).unwrap();
        let reader = png::Decoder::new(Cursor::new(std::fs::read(output).unwrap()))
            .read_info()
            .unwrap();
        assert_eq!(reader.info().color_type, png::ColorType::Rgba);
    }

    #[test]
    fn jpeg_and_heic_encode_decode_and_psnr() {
        let dir = TestDir::new();
        let original = sample();
        for format in [OutputFormat::Jpeg, OutputFormat::Heic] {
            let path = dir.path(&format!("roundtrip.{format}"));
            encode(&original, &path, format, 0.9).unwrap();
            let decoded = decode(&path, original.width * original.height).unwrap();
            assert_eq!(decoded.alpha, AlphaChannel::Absent, "{format}");
            assert_eq!(
                (decoded.width, decoded.height),
                (original.width, original.height)
            );
            let mut square_error = 0.0;
            for (before, after) in original
                .pixels
                .chunks_exact(4)
                .zip(decoded.pixels.chunks_exact(4))
            {
                assert_eq!(after[3], 255);
                for c in 0..3 {
                    square_error += (f64::from(before[c]) - f64::from(after[c])).powi(2);
                }
            }
            let mse = square_error
                / f64::from(
                    u32::try_from(original.width * original.height * 3)
                        .expect("small fixture fits u32"),
                );
            let psnr = 10.0 * (255.0 * 255.0 / mse).log10();
            eprintln!("codec {format}: round-trip PSNR {psnr:.3} dB");
            assert!(psnr >= 30.0, "{format}: {psnr} dB");
        }
    }

    fn jpeg_orientation(bytes: &[u8], orientation: u8) -> Vec<u8> {
        // APP1 EXIF: little-endian TIFF with one SHORT Orientation field.
        let mut exif = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
        exif.extend_from_slice(&[orientation, 0, 0, 0, 0, 0, 0, 0]);
        let mut result = bytes[..2].to_vec();
        result.extend_from_slice(&[0xff, 0xe1]);
        result.extend_from_slice(
            &u16::try_from(exif.len() + 2)
                .expect("small EXIF fixture fits u16")
                .to_be_bytes(),
        );
        result.extend_from_slice(&exif);
        result.extend_from_slice(&bytes[2..]);
        result
    }

    #[test]
    fn all_eight_exif_orientations_apply_once() {
        let dir = TestDir::new();
        let baseline_path = dir.path("orientation-base.jpg");
        encode(&sample(), &baseline_path, OutputFormat::Jpeg, 1.0).unwrap();
        let bytes = std::fs::read(&baseline_path).unwrap();
        let baseline = decode(&baseline_path, 64 * 48).unwrap();
        for orientation in 1..=8 {
            let path = dir.path(&format!("orientation-{orientation}.jpg"));
            std::fs::write(&path, jpeg_orientation(&bytes, orientation)).unwrap();
            let decoded = decode(&path, 64 * 48).unwrap();
            let (w, h) = (baseline.width, baseline.height);
            let expected_size = if orientation >= 5 { (h, w) } else { (w, h) };
            assert_eq!((decoded.width, decoded.height), expected_size);
            // Explicit source-coordinate lookup, independent of the image
            // backend's orientation transform implementation.
            for y in 0..decoded.height {
                for x in 0..decoded.width {
                    let (sx, sy) = match orientation {
                        1 => (x, y),
                        2 => (w - 1 - x, y),
                        3 => (w - 1 - x, h - 1 - y),
                        4 => (x, h - 1 - y),
                        5 => (y, x),
                        6 => (y, h - 1 - x),
                        7 => (w - 1 - y, h - 1 - x),
                        8 => (w - 1 - y, x),
                        _ => unreachable!("fixture orientation is in 1..=8"),
                    };
                    assert_eq!(
                        &decoded.pixels[(y * decoded.width + x) * 4..][..4],
                        &baseline.pixels[(sy * w + sx) * 4..][..4],
                        "orientation {orientation}, {x},{y}"
                    );
                }
            }
            let normalized = dir.path(&format!("normalized-{orientation}.png"));
            encode(&decoded, &normalized, OutputFormat::Png, 0.9).unwrap();
            assert_eq!(decode(&normalized, w * h).unwrap(), decoded);
        }
    }

    #[test]
    fn corrupt_and_unsupported_files_fail() {
        let dir = TestDir::new();
        let garbage = dir.path("garbage.png");
        std::fs::write(&garbage, b"this is not an image").unwrap();
        assert!(decode(&garbage, 100).is_err());
        let missing = decode(&dir.path("missing.png"), 100).unwrap_err();
        assert!(
            missing
                .source()
                .and_then(|source| source.downcast_ref::<std::io::Error>())
                .is_some(),
            "file I/O cause must survive the codec boundary: {missing:?}"
        );
        let gif = dir.path("renamed.png");
        std::fs::write(&gif, b"GIF89a\x01\0\x01\0\x80\0\0\0\0\0\xff\xff\xff!\xf9\x04\x01\0\0\0\0,\0\0\0\0\x01\0\x01\0\0\x02\x02D\x01\0;").unwrap();
        assert!(
            decode(&gif, 100)
                .unwrap_err()
                .to_string()
                .contains("unsupported")
        );
        let jpeg = dir.path("truncated.jpg");
        encode(&sample(), &jpeg, OutputFormat::Jpeg, 0.9).unwrap();
        let mut bytes = std::fs::read(&jpeg).unwrap();
        bytes.truncate(bytes.len() / 2);
        std::fs::write(&jpeg, bytes).unwrap();
        assert!(decode(&jpeg, 10000).is_err());
    }

    #[test]
    fn invalid_encoding_and_nul_paths_fail_safely() {
        let dir = TestDir::new();
        let image = sample();
        assert!("tiff".parse::<OutputFormat>().is_err());
        assert!(encode(&image, &dir.path("x.png"), OutputFormat::Png, f64::NAN).is_err());
        assert!(encode(&image, &dir.path("x.png"), OutputFormat::Png, -0.1).is_err());
        let invalid = RgbaImage {
            width: usize::MAX,
            height: 2,
            pixels: vec![],
            alpha: AlphaChannel::Absent,
        };
        assert!(encode(&invalid, &dir.path("x.png"), OutputFormat::Png, 0.9).is_err());
        let invalid_path = Path::new("bad\0path.png");
        assert!(
            decode(invalid_path, 10)
                .unwrap_err()
                .to_string()
                .contains("NUL")
        );
        assert!(
            encode(&image, invalid_path, OutputFormat::Png, 0.9)
                .unwrap_err()
                .to_string()
                .contains("NUL")
        );
        let mut alpha = image;
        alpha.pixels[3] = 128;
        alpha.alpha = AlphaChannel::Present;
        for format in [OutputFormat::Jpeg, OutputFormat::Heic] {
            assert!(
                encode(&alpha, &dir.path("alpha.jpg"), format, 0.9)
                    .unwrap_err()
                    .to_string()
                    .contains("opaque")
            );
        }
    }

    #[test]
    fn jpeg_accepts_trailing_bytes_but_not_fake_eoi_in_metadata() {
        let dir = TestDir::new();
        let path = dir.path("trailing.jpg");
        encode(&sample(), &path, OutputFormat::Jpeg, 0.9).unwrap();
        let expected = decode(&path, 64 * 48).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(b"\0padding after a valid JPEG end marker\0");
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(decode(&path, 64 * 48).unwrap(), expected);
        assert!(!jpeg_has_eoi(&[
            0xff, 0xd8, 0xff, 0xe1, 0, 6, 0xff, 0xd9, 0, 0
        ]));
        assert!(jpeg_has_eoi(&[
            0xff, 0xd8, 0xff, 0xda, 0, 2, 1, 0xff, 0, 2, 0xff, 0xd0, 3, 0xff, 0xd9, 4
        ]));
    }

    fn p3_to_srgb(pixel: &[u8]) -> [u8; 3] {
        fn linear(value: u8) -> f64 {
            let value = f64::from(value) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        }
        fn encoded(value: f64) -> u8 {
            let value = if value <= 0.003_130_8 {
                12.92 * value
            } else {
                1.055 * value.powf(1.0 / 2.4) - 0.055
            };
            rounded_byte(value.clamp(0.0, 1.0) * 255.0)
        }
        let [r, g, b] = [linear(pixel[0]), linear(pixel[1]), linear(pixel[2])];
        // Independent D65 Display-P3 -> linear-sRGB matrix, followed by the
        // IEC sRGB transfer function. ICC transformation code is not reused.
        [
            encoded(1.224_940_176 * r - 0.224_940_176 * g),
            encoded(-0.042_056_955 * r + 1.042_056_955 * g),
            encoded(-0.019_637_555 * r - 0.078_636_046 * g + 1.098_273_601 * b),
        ]
    }

    fn write_profiled_png(path: &Path, pixels: &[u8], profile: &ColorProfile) {
        let mut encoder = image::codecs::png::PngEncoder::new(File::create(path).unwrap());
        encoder.set_icc_profile(profile.encode().unwrap()).unwrap();
        encoder
            .write_image(
                pixels,
                u32::try_from(pixels.len() / 4).expect("test fixture width fits u32"),
                1,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
    }

    #[test]
    fn embedded_display_p3_icc_is_converted_to_srgb() {
        let dir = TestDir::new();
        let path = dir.path("display-p3.png");
        let pixels = [128, 80, 160, 255, 20, 200, 60, 128, 200, 40, 60, 255];
        write_profiled_png(&path, &pixels, &ColorProfile::new_display_p3());
        let decoded = decode(&path, 3).unwrap();
        assert_ne!(decoded.pixels, pixels);
        for (source, actual) in pixels.chunks_exact(4).zip(decoded.pixels.chunks_exact(4)) {
            let expected = p3_to_srgb(source);
            for c in 0..3 {
                assert!(
                    actual[c].abs_diff(expected[c]) <= 1,
                    "source {source:?}, actual {actual:?}, expected {expected:?}"
                );
            }
            assert_eq!(actual[3], source[3]);
        }
        let mut source = sample();
        let mut cmyk = ColorProfile::new_srgb();
        cmyk.color_space = DataColorSpace::Cmyk;
        assert!(
            convert_profile(&mut source, &cmyk)
                .unwrap_err()
                .to_string()
                .contains("unsupported")
        );
        let pq = dir.path("icc-pq.png");
        write_profiled_png(&pq, &pixels, &ColorProfile::new_display_p3_pq());
        assert!(decode(&pq, 3).unwrap_err().to_string().contains("HDR"));
    }

    #[test]
    fn grayscale_and_16bit_sdr_png_decode() {
        let dir = TestDir::new();
        for (name, depth, input) in [
            ("gray8.png", png::BitDepth::Eight, vec![0, 128, 255]),
            (
                "gray16.png",
                png::BitDepth::Sixteen,
                vec![0, 0, 0x80, 0, 0xff, 0xff],
            ),
        ] {
            let path = dir.path(name);
            let mut encoder = png::Encoder::new(File::create(&path).unwrap(), 3, 1);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(depth);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&input)
                .unwrap();
            let decoded = decode(&path, 3).unwrap();
            assert_eq!(
                decoded.pixels,
                [0, 0, 0, 255, 128, 128, 128, 255, 255, 255, 255, 255]
            );
        }
        let gray_profile = ColorProfile::new_gray_with_gamma(1.0);
        let path = dir.path("gray-icc.png");
        let mut info = png::Info::with_size(1, 1);
        info.color_type = png::ColorType::Grayscale;
        info.icc_profile = Some(gray_profile.encode().unwrap().into());
        let encoder = png::Encoder::with_info(File::create(&path).unwrap(), info).unwrap();
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[128])
            .unwrap();
        let decoded = decode(&path, 1).unwrap();
        assert!(decoded.pixels[0].abs_diff(188) <= 1);
        assert_eq!(decoded.pixels[0], decoded.pixels[1]);
        assert_eq!(decoded.pixels[1], decoded.pixels[2]);
    }

    #[test]
    fn png_color_priority_gamma_and_hdr_are_explicit() {
        let dir = TestDir::new();
        let path = dir.path("linear-gamma.png");
        let mut encoder = png::Encoder::new(File::create(&path).unwrap(), 1, 1);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_source_gamma(png::ScaledFloat::new(1.0));
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[128, 128, 128])
            .unwrap();
        let decoded = decode(&path, 1).unwrap();
        for value in &decoded.pixels[..3] {
            assert!(value.abs_diff(188) <= 1);
        }

        let p3 = dir.path("cicp-p3.png");
        let mut info = png::Info::with_size(1, 1);
        info.color_type = png::ColorType::Rgb;
        // Lower-priority linear gamma must not override cICP Display P3.
        info.source_gamma = Some(png::ScaledFloat::new(1.0));
        let mut writer = png::Encoder::with_info(File::create(&p3).unwrap(), info)
            .unwrap()
            .write_header()
            .unwrap();
        // png 0.18 parses cICP but its encoder does not emit the Info field.
        writer
            .write_chunk(png::chunk::ChunkType(*b"cICP"), &[12, 13, 0, 1])
            .unwrap();
        writer.write_image_data(&[128, 80, 160]).unwrap();
        writer.finish().unwrap();
        let actual = decode(&p3, 1).unwrap();
        for (value, expected) in actual.pixels[..3].iter().zip(p3_to_srgb(&[128, 80, 160])) {
            assert!(
                value.abs_diff(expected) <= 1,
                "actual {:?}, expected {:?}",
                actual.pixels,
                p3_to_srgb(&[128, 80, 160])
            );
        }
        for transfer_function in [16, 18] {
            let hdr = dir.path(&format!("hdr-{transfer_function}.png"));
            let mut info = png::Info::with_size(1, 1);
            info.color_type = png::ColorType::Rgb;
            info.icc_profile = Some(ColorProfile::new_srgb().encode().unwrap().into());
            let mut writer = png::Encoder::with_info(File::create(&hdr).unwrap(), info)
                .unwrap()
                .write_header()
                .unwrap();
            writer
                .write_chunk(
                    png::chunk::ChunkType(*b"cICP"),
                    &[9, transfer_function, 0, 1],
                )
                .unwrap();
            writer.write_image_data(&[128, 128, 128]).unwrap();
            writer.finish().unwrap();
            assert!(decode(&hdr, 1).unwrap_err().to_string().contains("HDR"));
        }
    }

    fn oriented_heic(path: &Path, image: &RgbaImage, exif: i32, container: i32) {
        use oxideav_heif::{Imir, Irot, Property};
        let mut transforms = Vec::new();
        match container {
            2 => transforms.push(Property::Imir(Imir::new(1))),
            3 => transforms.push(Property::Irot(Irot::new(2))),
            4 => transforms.push(Property::Imir(Imir::new(0))),
            5 => {
                transforms.push(Property::Irot(Irot::new(3)));
                transforms.push(Property::Imir(Imir::new(1)));
            }
            6 => transforms.push(Property::Irot(Irot::new(3))),
            7 => {
                transforms.push(Property::Irot(Irot::new(3)));
                transforms.push(Property::Imir(Imir::new(0)));
            }
            8 => transforms.push(Property::Irot(Irot::new(1))),
            _ => {}
        }
        let exif = if exif == 1 {
            None
        } else {
            let mut bytes = b"II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
            bytes.extend_from_slice(&[
                u8::try_from(exif).expect("fixture orientation is in 1..=8"),
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            ]);
            Some(bytes)
        };
        let output = encode_heic(
            image,
            ColorProfile::new_srgb().encode().unwrap(),
            0.9,
            exif,
            transforms,
        )
        .unwrap();
        std::fs::write(path, output).unwrap();
    }

    #[test]
    fn heic_singletons_odd_sizes_and_external_encoder() {
        let dir = TestDir::new();
        for (width, height) in [(1, 1), (1, 7), (7, 1), (13, 9)] {
            let image = RgbaImage {
                width,
                height,
                pixels: [80, 160, 220, 255].repeat(width * height),
                alpha: AlphaChannel::Absent,
            };
            let path = dir.path(&format!("tiny-{width}-{height}.heic"));
            encode(&image, &path, OutputFormat::Heic, 0.9).unwrap();
            let decoded = decode(&path, width * height).unwrap();
            assert_eq!((decoded.width, decoded.height), (width, height));
        }
        let independent =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kvazaar-smooth.heic");
        let decoded = decode(&independent, 64 * 48).unwrap();
        assert_eq!((decoded.width, decoded.height), (64, 48));
        let expected = sample();
        let squared_error: f64 = expected
            .pixels
            .chunks_exact(4)
            .zip(decoded.pixels.chunks_exact(4))
            .flat_map(|(a, b)| (0..3).map(move |c| (f64::from(a[c]) - f64::from(b[c])).powi(2)))
            .sum();
        let psnr = 10.0 * (255.0 * 255.0 / (squared_error / (64.0 * 48.0 * 3.0))).log10();
        assert!(psnr >= 30.0, "independent Kvazaar fixture PSNR {psnr}");
        if let Some(external) = std::env::var_os("RRO_EXTERNAL_HEIC") {
            let image = decode(Path::new(&external), 2_000_000).unwrap();
            assert_eq!((image.width, image.height), (1280, 854));
        }
    }

    #[test]
    fn heic_without_nclx_uses_limited_range_from_sps_vui() {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let decoded = decode(&fixtures.join("x265-limited-no-nclx.heic"), 64 * 48).unwrap();
        let expected = image::open(fixtures.join("x265-limited-ffmpeg.png"))
            .unwrap()
            .into_rgba8();
        assert_eq!((decoded.width, decoded.height), (64, 48));
        let worst = decoded
            .pixels
            .iter()
            .zip(expected.as_raw())
            .map(|(&actual, &expected)| actual.abs_diff(expected))
            .max()
            .unwrap();
        assert!(
            worst <= 2,
            "SPS VUI limited-range decode differs from independent FFmpeg output by {worst} codes"
        );
    }

    #[test]
    fn heic_derivation_canvas_is_bounded_independently_of_ispe() {
        let bytes = include_bytes!("../tests/fixtures/kvazaar-smooth.heic");
        let file = oxideav_heif::HeifFile::parse_borrowed(bytes).unwrap();
        let mut node = oxideav_heif::build_primary_graph(&file).unwrap();
        // A malicious descriptor may disagree with a small declared ispe;
        // the compositor allocates the descriptor's canvas before transforms.
        for kind in [
            oxideav_heif::ImageKind::Grid(oxideav_heif::GridDescriptor::new(1, 1, 1024, 1024)),
            oxideav_heif::ImageKind::Overlay(oxideav_heif::OverlayDescriptor::new(
                [0; 4],
                1024,
                1024,
                Vec::new(),
            )),
        ] {
            node.kind = kind;
            assert!(
                heic_decode_limits(&file, &node, 1024 * 1024)
                    .unwrap_err()
                    .to_string()
                    .contains("canvas"),
                "the raw derivation canvas must be checked even when ispe is small"
            );
            node.properties
                .entries
                .retain(|entry| !matches!(entry.property, oxideav_heif::Property::Ispe(_)));
            assert!(
                heic_decode_limits(&file, &node, 1024 * 1024)
                    .unwrap_err()
                    .to_string()
                    .contains("canvas"),
                "an absent ispe must not bypass raw canvas validation"
            );
        }
    }

    #[test]
    fn heic_derivation_canvases_share_one_memory_budget() {
        let bytes = include_bytes!("../tests/fixtures/kvazaar-smooth.heic");
        let file = oxideav_heif::HeifFile::parse_borrowed(bytes).unwrap();
        let mut node = oxideav_heif::build_primary_graph(&file).unwrap();
        node.kind =
            oxideav_heif::ImageKind::Grid(oxideav_heif::GridDescriptor::new(1, 1, 400, 400));
        node.inputs = vec![node.clone()];
        assert!(
            heic_decode_limits(&file, &node, 2 * 1024 * 1024)
                .unwrap_err()
                .to_string()
                .contains("memory"),
            "two individually bounded live canvases must not each consume the full budget"
        );
    }

    #[test]
    fn heic_scale_transforms_fail_before_decoder_allocation() {
        let bytes = include_bytes!("../tests/fixtures/kvazaar-smooth.heic");
        let file = oxideav_heif::HeifFile::parse_borrowed(bytes).unwrap();
        let mut node = oxideav_heif::build_primary_graph(&file).unwrap();
        let mut scale = node.properties.entries[0].clone();
        scale.property =
            oxideav_heif::Property::Iscl(oxideav_heif::props::Iscl::new(1000, 1, 1000, 1));
        node.properties.entries.push(scale);
        assert!(
            heic_decode_limits(&file, &node, 1024 * 1024)
                .unwrap_err()
                .to_string()
                .contains("scaling"),
            "unsupported scaling must not expand before a later crop hides the allocation"
        );
    }

    #[test]
    fn heic_repeated_overlay_inputs_share_one_memory_budget() {
        let bytes = include_bytes!("../tests/fixtures/kvazaar-smooth.heic");
        let file = oxideav_heif::HeifFile::parse_borrowed(bytes).unwrap();
        let mut node = oxideav_heif::build_primary_graph(&file).unwrap();
        let coded = node.clone();
        node.kind = oxideav_heif::ImageKind::Overlay(oxideav_heif::OverlayDescriptor::new(
            [0; 4],
            64,
            48,
            vec![(0, 0); 16],
        ));
        node.inputs = vec![coded; 16];
        assert!(
            heic_decode_limits(&file, &node, 1024 * 1024)
                .unwrap_err()
                .to_string()
                .contains("coded reconstructions"),
            "each retained reconstruction must count even when the coded item ID repeats"
        );
    }

    #[test]
    fn heic_coded_sources_and_canvases_share_one_memory_budget() {
        let bytes = include_bytes!("../tests/fixtures/kvazaar-smooth.heic");
        let file = oxideav_heif::HeifFile::parse_borrowed(bytes).unwrap();
        let mut node = oxideav_heif::build_primary_graph(&file).unwrap();
        let coded = node.clone();
        node.kind =
            oxideav_heif::ImageKind::Grid(oxideav_heif::GridDescriptor::new(1, 1, 512, 512));
        node.inputs = vec![coded];
        assert!(
            heic_decode_limits(&file, &node, 2 * 1024 * 1024)
                .unwrap_err()
                .to_string()
                .contains("coded items and derivation canvases"),
            "coded-source and derived-canvas estimates must be added together"
        );
    }

    #[test]
    fn heic_public_decode_rejects_hidden_oversized_canvas() {
        use oxideav_heif::{HeifWriter, Ispe, OverlayDescriptor, Property};
        let bytes = include_bytes!("../tests/fixtures/kvazaar-smooth.heic");
        let file = oxideav_heif::HeifFile::parse_borrowed(bytes).unwrap();
        let node = oxideav_heif::build_primary_graph(&file).unwrap();
        let mut writer = HeifWriter::new();
        let coded = writer.add_coded_item(
            *b"hvc1",
            file.item_data(node.item.id).unwrap().into_owned(),
            node.properties
                .entries
                .into_iter()
                .map(|entry| (entry.property, entry.essential))
                .collect(),
        );
        let overlay = writer
            .add_overlay(
                OverlayDescriptor::new([0; 4], 512, 512, vec![(0, 0)]),
                &[coded],
                vec![(Property::Ispe(Ispe::new(1, 1)), false)],
            )
            .unwrap();
        writer.set_primary(overlay);
        let dir = TestDir::new();
        let path = dir.path("oversized-hidden-canvas.heic");
        std::fs::write(&path, writer.write_to_vec().unwrap()).unwrap();
        assert!(
            decode(&path, 1)
                .unwrap_err()
                .to_string()
                .contains("derivation canvases"),
            "the public decode path must reject descriptor allocation before the dependency decodes"
        );
    }

    #[test]
    fn heic_exif_fallback_and_container_orientation_apply_once() {
        let dir = TestDir::new();
        let image = sample();
        let baseline_path = dir.path("base.heic");
        oriented_heic(&baseline_path, &image, 1, 1);
        let baseline = decode(&baseline_path, image.width * image.height).unwrap();
        for orientation in 2..=8 {
            for (exif, container) in [
                (orientation, 1),
                (1, orientation),
                (orientation, orientation),
            ] {
                let path = dir.path(&format!("orientation-{exif}-{container}.heic"));
                oriented_heic(&path, &image, exif, container);
                let decoded = decode(&path, image.width * image.height).unwrap();
                let (w, h) = (baseline.width, baseline.height);
                let expected_size = if orientation >= 5 { (h, w) } else { (w, h) };
                assert_eq!((decoded.width, decoded.height), expected_size);
                for y in 0..decoded.height {
                    for x in 0..decoded.width {
                        let (sx, sy) = match orientation {
                            2 => (w - 1 - x, y),
                            3 => (w - 1 - x, h - 1 - y),
                            4 => (x, h - 1 - y),
                            5 => (y, x),
                            6 => (y, h - 1 - x),
                            7 => (w - 1 - y, h - 1 - x),
                            8 => (w - 1 - y, x),
                            _ => unreachable!("fixture orientation is in 2..=8"),
                        };
                        assert_eq!(
                            &decoded.pixels[(y * decoded.width + x) * 4..][..4],
                            &baseline.pixels[(sy * baseline.width + sx) * 4..][..4],
                            "Exif/container {exif}/{container}"
                        );
                    }
                }
            }
        }
    }
}
