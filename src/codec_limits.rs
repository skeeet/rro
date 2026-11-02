//! Allocation-free structural limits applied before the HEIF crate parser.
//!
//! In particular, `iloc` permits zero-width extent fields. A tiny encoded
//! table can therefore expand into many in-memory `Extent` records; limiting
//! only file bytes or the decoded image dimensions does not bound that cost.
use super::CodecError;

/// Bound box traversal work before building any typed container structures.
const MAX_BOXES: usize = 16_384;
/// Bound item/location/entity allocations for normal still-image containers.
const MAX_ITEMS: usize = 4096;
/// Bound cumulative extent records, including zero-width encoded entries.
const MAX_EXTENTS: usize = 65_536;
/// Prevent one small item table from dominating the global allocation budget.
const MAX_ITEM_EXTENTS: usize = 16_384;
/// Bound the cumulative number of derived-image and auxiliary graph edges.
const MAX_REFERENCES: usize = 65_536;
/// Bound property association records before typed property cloning.
const MAX_ASSOCIATIONS: usize = 65_536;
/// Bound HEVC configuration entries before their individual payloads are parsed.
const MAX_CONFIG_NALS: usize = 4096;
/// Bound structural metadata while keeping coded image payloads opaque.
const MAX_METADATA_BYTES: usize = 16 * 1024 * 1024;
/// Standard metadata nesting is shallow; cap malformed recursive containers.
const MAX_DEPTH: usize = 16;

#[derive(Default)]
struct Budget {
    boxes: usize,
    items: usize,
    locations: usize,
    extents: usize,
    references: usize,
    associations: usize,
    groups: usize,
    config_nals: usize,
    metadata_bytes: usize,
}

fn add(value: &mut usize, amount: usize, limit: usize, what: &str) -> Result<(), CodecError> {
    *value = value
        .checked_add(amount)
        .ok_or_else(|| format!("HEIF {what} count overflow"))?;
    if *value > limit {
        return Err(CodecError::invalid(format!(
            "HEIF {what} exceeds the supported limit of {limit}"
        )));
    }
    Ok(())
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }
    fn take(&mut self, count: usize) -> Result<&'a [u8], CodecError> {
        let end = self
            .at
            .checked_add(count)
            .ok_or("HEIF structural size overflow")?;
        let result = self
            .bytes
            .get(self.at..end)
            .ok_or("truncated HEIF metadata table")?;
        self.at = end;
        Ok(result)
    }
    fn skip(&mut self, count: usize) -> Result<(), CodecError> {
        self.take(count).map(|_| ())
    }
    fn u8(&mut self) -> Result<u8, CodecError> {
        Ok(self.take(1)?[0])
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], CodecError> {
        self.take(N)?
            .try_into()
            .map_err(|error| CodecError::with_source("invalid HEIF fixed-size field", error))
    }
    fn u16(&mut self) -> Result<usize, CodecError> {
        Ok(usize::from(u16::from_be_bytes(self.array()?)))
    }
    fn u32(&mut self) -> Result<usize, CodecError> {
        Ok(usize::try_from(u32::from_be_bytes(self.array()?))?)
    }
    fn rest(&self) -> &'a [u8] {
        &self.bytes[self.at..]
    }
}

fn full_box(bytes: &[u8]) -> Result<(u8, u32, &[u8]), CodecError> {
    let header = bytes.get(..4).ok_or("truncated HEIF FullBox header")?;
    let flags = u32::from_be_bytes([0, header[1], header[2], header[3]]);
    Ok((header[0], flags, &bytes[4..]))
}

#[derive(Clone, Copy)]
enum Context {
    Top,
    Meta,
    ItemInfo,
    Properties,
    PropertyValues,
    References(u8),
    Groups,
    DataInfo,
    DataReferences,
}

fn walk(
    bytes: &[u8],
    context: Context,
    depth: usize,
    budget: &mut Budget,
) -> Result<(), CodecError> {
    if depth > MAX_DEPTH {
        return Err("HEIF metadata nesting is too deep".into());
    }
    let mut reader = Reader::new(bytes);
    while !reader.rest().is_empty() {
        let remaining = reader.rest().len();
        let short_size = reader.u32()?;
        let kind: [u8; 4] = reader.array()?;
        let mut header = 8usize;
        let size = if short_size == 1 {
            header = 16;
            let value = u64::from_be_bytes(reader.array()?);
            usize::try_from(value).map_err(|_| "HEIF box size exceeds the address space")?
        } else if short_size == 0 {
            remaining
        } else {
            short_size
        };
        if kind == *b"uuid" {
            reader.skip(16)?;
            header += 16;
        }
        if size < header || size > remaining {
            return Err("invalid or truncated HEIF box size".into());
        }
        let payload = reader.take(size - header)?;
        add(&mut budget.boxes, 1, MAX_BOXES, "box count")?;
        // A minimized-image box expands into a second in-memory container
        // before ordinary limits can be checked. It is not a v1 input path.
        if kind == *b"mini" {
            return Err("minimized HEIF containers are not supported".into());
        }
        inspect_payload(payload, kind, context, depth, budget)?;
    }
    Ok(())
}

fn inspect_payload(
    payload: &[u8],
    kind: [u8; 4],
    context: Context,
    depth: usize,
    budget: &mut Budget,
) -> Result<(), CodecError> {
    match context {
        Context::Top => {
            if kind == *b"meta" {
                let (version, _, children) = full_box(payload)?;
                if version != 0 {
                    return Err("unsupported HEIF meta version".into());
                }
                walk(children, Context::Meta, depth + 1, budget)?;
            }
        }
        Context::Meta => inspect_meta(payload, kind, depth, budget)?,

        Context::Properties => match &kind {
            b"ipco" => walk(payload, Context::PropertyValues, depth + 1, budget)?,
            b"ipma" => {
                metadata(budget, payload.len())?;
                associations(payload, budget)?;
            }
            _ => metadata(budget, payload.len())?,
        },
        Context::References(version) => {
            metadata(budget, payload.len())?;
            let mut table = Reader::new(payload);
            table.skip(if version == 0 { 2 } else { 4 })?;
            let count = table.u16()?;
            add(
                &mut budget.references,
                count,
                MAX_REFERENCES,
                "reference target count",
            )?;
            table.skip(count * if version == 0 { 2 } else { 4 })?;
        }
        Context::Groups => {
            metadata(budget, payload.len())?;
            add(&mut budget.groups, 1, MAX_ITEMS, "entity group count")?;
            let (_, _, body) = full_box(payload)?;
            let mut table = Reader::new(body);
            table.skip(4)?;
            let count = table.u32()?;
            add(
                &mut budget.references,
                count,
                MAX_REFERENCES,
                "reference target count",
            )?;
            table.skip(count * 4)?;
        }
        Context::DataInfo => {
            if kind == *b"dref" {
                let (_, _, body) = full_box(payload)?;
                let mut table = Reader::new(body);
                let count = table.u32()?;
                if count > MAX_ITEMS {
                    return Err("too many HEIF data references".into());
                }
                walk(table.rest(), Context::DataReferences, depth + 1, budget)?;
            } else {
                metadata(budget, payload.len())?;
            }
        }
        Context::PropertyValues if kind == *b"hvcC" => {
            metadata(budget, payload.len())?;
            hevc_configuration(payload, budget)?;
        }
        Context::ItemInfo | Context::PropertyValues | Context::DataReferences => {
            metadata(budget, payload.len())?;
        }
    }
    Ok(())
}

fn inspect_meta(
    payload: &[u8],
    kind: [u8; 4],
    depth: usize,
    budget: &mut Budget,
) -> Result<(), CodecError> {
    match &kind {
        b"iinf" => {
            let (version, _, body) = full_box(payload)?;
            let mut table = Reader::new(body);
            let count = if version == 0 {
                table.u16()?
            } else {
                table.u32()?
            };
            add(&mut budget.items, count, MAX_ITEMS, "item count")?;
            walk(table.rest(), Context::ItemInfo, depth + 1, budget)?;
        }
        b"iloc" => {
            metadata(budget, payload.len())?;
            locations(payload, budget)?;
        }
        b"iprp" => walk(payload, Context::Properties, depth + 1, budget)?,
        b"iref" => {
            let (version, _, children) = full_box(payload)?;
            if version > 1 {
                return Err("unsupported HEIF iref version".into());
            }
            walk(children, Context::References(version), depth + 1, budget)?;
        }
        b"grpl" => walk(payload, Context::Groups, depth + 1, budget)?,
        b"dinf" => walk(payload, Context::DataInfo, depth + 1, budget)?,
        b"idat" => (), // Coded bytes, already bounded by the input-file limit.
        b"ipro" => {
            metadata(budget, payload.len())?;
            let (_, _, body) = full_box(payload)?;
            let schemes = Reader::new(body).u16()?;
            if schemes > MAX_ITEMS {
                return Err("too many HEIF protection schemes".into());
            }
        }
        _ => metadata(budget, payload.len())?,
    }
    Ok(())
}

fn metadata(budget: &mut Budget, bytes: usize) -> Result<(), CodecError> {
    add(
        &mut budget.metadata_bytes,
        bytes,
        MAX_METADATA_BYTES,
        "structural metadata bytes",
    )
}

fn locations(bytes: &[u8], budget: &mut Budget) -> Result<(), CodecError> {
    let (version, _, body) = full_box(bytes)?;
    if version > 2 {
        return Err("unsupported HEIF iloc version".into());
    }
    let mut reader = Reader::new(body);
    let sizes = reader.u8()?;
    let second = reader.u8()?;
    let offset_size = usize::from(sizes >> 4);
    let length_size = usize::from(sizes & 15);
    let base_size = usize::from(second >> 4);
    let index_size = if version >= 1 {
        usize::from(second & 15)
    } else {
        0
    };
    for size in [offset_size, length_size, base_size, index_size] {
        if !matches!(size, 0 | 4 | 8) {
            return Err("unsupported HEIF iloc field width".into());
        }
    }
    let count = if version < 2 {
        reader.u16()?
    } else {
        reader.u32()?
    };
    add(&mut budget.locations, count, MAX_ITEMS, "location count")?;
    for _ in 0..count {
        reader.skip(if version < 2 { 2 } else { 4 })?;
        if version >= 1 {
            reader.skip(2)?;
        }
        reader.skip(2 + base_size)?;
        let extents = reader.u16()?;
        if extents > MAX_ITEM_EXTENTS {
            return Err(CodecError::invalid(format!(
                "HEIF item has more than {MAX_ITEM_EXTENTS} extents"
            )));
        }
        add(&mut budget.extents, extents, MAX_EXTENTS, "extent count")?;
        // This calculation is bounded by the caps above, even when every
        // encoded field width is zero and no bytes are consumed per extent.
        reader.skip(extents * (index_size + offset_size + length_size))?;
    }
    Ok(())
}

fn associations(bytes: &[u8], budget: &mut Budget) -> Result<(), CodecError> {
    let (version, flags, body) = full_box(bytes)?;
    if version > 1 {
        return Err("unsupported HEIF ipma version".into());
    }
    let mut reader = Reader::new(body);
    let count = reader.u32()?;
    if count > MAX_ITEMS {
        return Err("too many HEIF property-association rows".into());
    }
    for _ in 0..count {
        reader.skip(if version == 0 { 2 } else { 4 })?;
        let associations = usize::from(reader.u8()?);
        add(
            &mut budget.associations,
            associations,
            MAX_ASSOCIATIONS,
            "property-association count",
        )?;
        reader.skip(associations * if flags & 1 != 0 { 2 } else { 1 })?;
    }
    Ok(())
}

fn hevc_configuration(bytes: &[u8], budget: &mut Budget) -> Result<(), CodecError> {
    let mut reader = Reader::new(bytes);
    reader.skip(22)?;
    let arrays = reader.u8()?;
    for _ in 0..arrays {
        reader.skip(1)?;
        let count = reader.u16()?;
        add(
            &mut budget.config_nals,
            count,
            MAX_CONFIG_NALS,
            "HEVC configuration NAL count",
        )?;
        for _ in 0..count {
            let length = reader.u16()?;
            reader.skip(length)?;
        }
    }
    Ok(())
}

/// Bound parser allocations before calling `HeifFile::parse_borrowed`.
/// Unknown box payloads remain opaque; image bitstreams are never box-walked.
pub(super) fn preflight(bytes: &[u8]) -> Result<(), CodecError> {
    walk(bytes, Context::Top, 0, &mut Budget::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn boxed(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = u32::try_from(payload.len() + 8)
            .expect("small fixture box fits u32")
            .to_be_bytes()
            .to_vec();
        out.extend_from_slice(&kind);
        out.extend_from_slice(payload);
        out
    }
    fn meta(children: &[u8]) -> Vec<u8> {
        let mut p = vec![0; 4];
        p.extend_from_slice(children);
        boxed(*b"meta", &p)
    }
    fn iloc(extents: &[u16]) -> Vec<u8> {
        let mut p = vec![0, 0, 0, 0, 0, 0];
        p.extend_from_slice(
            &u16::try_from(extents.len())
                .expect("fixture location count fits u16")
                .to_be_bytes(),
        );
        for (id, count) in extents.iter().enumerate() {
            p.extend_from_slice(
                &u16::try_from(id)
                    .expect("fixture item id fits u16")
                    .to_be_bytes(),
            );
            p.extend_from_slice(&0u16.to_be_bytes());
            p.extend_from_slice(&count.to_be_bytes());
        }
        meta(&boxed(*b"iloc", &p))
    }
    #[test]
    fn zero_width_extent_tables_are_bounded_before_allocation() {
        assert!(preflight(&iloc(&[3, 4])).is_ok());
        assert!(
            preflight(&iloc(&[16_385]))
                .unwrap_err()
                .to_string()
                .contains("extents")
        );
        assert!(
            preflight(&iloc(&[16_384; 5]))
                .unwrap_err()
                .to_string()
                .contains("extent count")
        );
        assert!(
            preflight(&iloc(&[0; 4097]))
                .unwrap_err()
                .to_string()
                .contains("location count")
        );
    }
    #[test]
    fn invalid_box_sizes_and_truncated_count_fields_fail() {
        assert!(preflight(&[0, 0, 0, 4, b'm', b'e', b't', b'a']).is_err());
        assert!(preflight(&meta(&boxed(*b"iloc", &[0; 4]))).is_err());
        assert!(preflight(&boxed(*b"uuid", &[0; 3])).is_err());
        let mut extended = vec![0, 0, 0, 1, b'm', b'd', b'a', b't'];
        extended.extend_from_slice(&u64::MAX.to_be_bytes());
        assert!(preflight(&extended).is_err());
    }
    #[test]
    fn coded_and_unknown_payloads_are_opaque() {
        assert!(preflight(&boxed(*b"mdat", b"notaboxmini\0\0\0")).is_ok());
        assert!(preflight(&boxed(*b"free", &boxed(*b"mini", &[]))).is_ok());
        assert!(
            preflight(&boxed(*b"mini", &[]))
                .unwrap_err()
                .to_string()
                .contains("minimized")
        );
        let mut zero_size = vec![0, 0, 0, 0, b'm', b'd', b'a', b't'];
        zero_size.extend_from_slice(&[1, 2, 3]);
        assert!(preflight(&zero_size).is_ok());
    }
    #[test]
    fn reference_and_box_counts_are_globally_bounded() {
        let mut refs = Vec::new();
        for _ in 0..2 {
            let mut p = 1u16.to_be_bytes().to_vec();
            p.extend_from_slice(&40_000u16.to_be_bytes());
            p.resize(p.len() + 80_000, 0);
            refs.extend_from_slice(&boxed(*b"dimg", &p));
        }
        let mut full = vec![0; 4];
        full.extend_from_slice(&refs);
        assert!(
            preflight(&meta(&boxed(*b"iref", &full)))
                .unwrap_err()
                .to_string()
                .contains("reference target")
        );
        let many = boxed(*b"free", &[]).repeat(MAX_BOXES + 1);
        assert!(
            preflight(&many)
                .unwrap_err()
                .to_string()
                .contains("box count")
        );
    }
    #[test]
    fn supported_iloc_versions_and_variable_fields_are_checked() {
        for version in [0, 1, 2] {
            let mut p = vec![version, 0, 0, 0, 0x44, 0x40];
            if version < 2 {
                p.extend_from_slice(&1u16.to_be_bytes());
            } else {
                p.extend_from_slice(&1u32.to_be_bytes());
            }
            if version < 2 {
                p.extend_from_slice(&1u16.to_be_bytes());
            } else {
                p.extend_from_slice(&1u32.to_be_bytes());
            }
            if version >= 1 {
                p.extend_from_slice(&0u16.to_be_bytes());
            }
            p.extend_from_slice(&0u16.to_be_bytes());
            p.extend_from_slice(&0u32.to_be_bytes());
            p.extend_from_slice(&1u16.to_be_bytes());
            p.extend_from_slice(&0u32.to_be_bytes());
            p.extend_from_slice(&12u32.to_be_bytes());
            assert!(
                preflight(&meta(&boxed(*b"iloc", &p))).is_ok(),
                "version={version}"
            );
            p.pop();
            assert!(preflight(&meta(&boxed(*b"iloc", &p))).is_err());
        }
    }

    #[test]
    fn hevc_configuration_nal_counts_are_limited_before_typed_parse() {
        let wrap = |payload: &[u8]| {
            meta(&boxed(
                *b"iprp",
                &boxed(*b"ipco", &boxed(*b"hvcC", payload)),
            ))
        };
        let mut p = vec![0; 22];
        p.push(1);
        p.push(33);
        p.extend_from_slice(&u16::MAX.to_be_bytes());
        assert!(
            preflight(&wrap(&p))
                .unwrap_err()
                .to_string()
                .contains("NAL count")
        );
        p.truncate(24);
        p.extend_from_slice(&1u16.to_be_bytes());
        p.extend_from_slice(&2u16.to_be_bytes());
        p.extend_from_slice(&[0x42, 1]);
        assert!(preflight(&wrap(&p)).is_ok());
        p.pop();
        assert!(preflight(&wrap(&p)).is_err());
    }
}
