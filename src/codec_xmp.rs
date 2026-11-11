//! Bounded namespace-aware XMP orientation fallback for raster codecs.
use super::CodecError;
use image::metadata::Orientation;

/// Bound XML input, including decompressed PNG iTXt packets.
pub(super) const MAX_XMP_BYTES: usize = 1024 * 1024;
/// Bound the XML tree independently of its serialized byte count.
const MAX_XMP_NODES: u32 = 16_384;
/// The TIFF property namespace is independent of the XML prefix spelling.
const TIFF_NAMESPACE: &str = "http://ns.adobe.com/tiff/1.0/";

pub(super) fn merge_orientation(
    orientation: &mut Option<Orientation>,
    candidate: Orientation,
) -> Result<(), CodecError> {
    if orientation.is_some_and(|existing| existing != candidate) {
        return Err(CodecError::invalid("conflicting XMP orientation values"));
    }
    *orientation = Some(candidate);
    Ok(())
}

pub(super) fn orientation(bytes: &[u8]) -> Result<Option<Orientation>, CodecError> {
    if bytes.len() > MAX_XMP_BYTES {
        return Err(CodecError::invalid("XMP metadata exceeds byte limit"));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|error| CodecError::with_source("XMP metadata is not UTF-8", error))?;
    let document = roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_XMP_NODES,
            ..roxmltree::ParsingOptions::default()
        },
    )
    .map_err(|error| CodecError::with_source("invalid XMP XML metadata", error))?;
    let mut orientation = None;
    for node in document.descendants().filter(roxmltree::Node::is_element) {
        let attribute = node.attribute((TIFF_NAMESPACE, "Orientation"));
        let element = if node.has_tag_name((TIFF_NAMESPACE, "Orientation"))
            && node.children().all(|child| !child.is_element())
        {
            Some(
                node.children()
                    .filter(roxmltree::Node::is_text)
                    .filter_map(|child| child.text())
                    .collect::<String>(),
            )
        } else {
            None
        };
        for value in [attribute, element.as_deref()].into_iter().flatten() {
            let value = value.trim();
            // Parse a positive integer property, never a signed/decimal string.
            if !value.bytes().all(|byte| byte.is_ascii_digit()) {
                continue;
            }
            if let Some(candidate) = value.parse::<u8>().ok().and_then(Orientation::from_exif) {
                merge_orientation(&mut orientation, candidate)?;
            }
        }
    }
    Ok(orientation)
}
