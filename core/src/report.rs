use alloc::vec::Vec;

pub const MAX_DESCRIPTOR: usize = 4096;
pub const MAX_REPORT_BYTES: usize = 4096;
const MAX_PUSHED: usize = 8;
const MAX_DEPTH: usize = 16;
const MAX_USAGE_RANGES: usize = 64;
const MAX_FIELDS: usize = 512;
const MAX_COLLECTIONS: usize = 256;
const MAX_ELEMENT_BITS: u32 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Input,
    Output,
    Feature,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Usage {
    pub page: u16,
    pub id: u16,
}

impl Usage {
    pub const fn new(page: u16, id: u16) -> Self {
        Usage { page, id }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub min: i64,
    pub max: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Collection {
    pub kind: u8,
    pub usage: Usage,
    pub parent: Option<usize>,
}

pub const APPLICATION: u8 = 1;
pub const LOGICAL: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub kind: Kind,
    pub report: u8,
    pub offset: u32,
    pub size: u32,
    pub count: u32,
    pub variable: bool,
    pub relative: bool,
    pub usages: Vec<(Usage, Usage)>,
    pub logical: Range,
    pub physical: Range,
    pub unit: u32,
    pub exponent: i8,
    pub collection: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Location {
    pub report: u8,
    pub offset: u32,
    pub size: u32,
    pub logical: Range,
    pub physical: Range,
    pub unit: u32,
    pub exponent: i8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    TooLong(usize),
    Truncated(usize),
    TooManyPushes(usize),
    PopWithoutPush(usize),
    TooDeep(usize),
    TooManyUsages(usize),
    TooManyFields(usize),
    TooManyCollections(usize),
    UnbalancedCollection(usize),
    UnclosedCollection,
    ElementTooWide(usize),
    ReportTooLong(usize),
    ZeroReportId(usize),
    MixedNumbering(usize),
    BadPage(usize),
}

#[derive(Debug, Clone, Default)]
struct Global {
    page: u16,
    logical_min: i64,
    logical_max: (i64, i64),
    physical_min: i64,
    physical_max: (i64, i64),
    exponent: i8,
    unit: u32,
    size: u32,
    report: u8,
    count: u32,
}

#[derive(Debug, Clone, Default)]
struct Local {
    usages: Vec<(Usage, Usage)>,
    minimum: Option<Usage>,
}

#[derive(Debug, Clone, Default)]
pub struct ReportDescriptor {
    pub collections: Vec<Collection>,
    pub fields: Vec<Field>,
    pub numbered: bool,
    sizes: Vec<(Kind, u8, u32)>,
}

fn signed(data: &[u8]) -> i64 {
    match *data {
        [] => 0,
        [a] => i64::from(a as i8),
        [a, b] => i64::from(i16::from_le_bytes([a, b])),
        [a, b, c, d] => i64::from(i32::from_le_bytes([a, b, c, d])),
        _ => 0,
    }
}

fn unsigned(data: &[u8]) -> u32 {
    data.iter().rev().fold(0, |value, byte| value << 8 | u32::from(*byte))
}

fn usage_of(data: &[u8], page: u16) -> Usage {
    let value = unsigned(data);
    if data.len() == 4 { Usage::new((value >> 16) as u16, value as u16) } else { Usage::new(page, value as u16) }
}

fn bounded(min: i64, max: (i64, i64)) -> Range {
    Range { min, max: if min < 0 { max.1 } else { max.0 } }
}

impl Range {
    pub fn contains(&self, value: i64) -> bool {
        self.min <= value && value <= self.max
    }
}

impl Field {
    pub fn usage(&self, element: u32) -> Option<Usage> {
        let mut skipped = 0u32;
        let mut last = None;
        for (first, end) in &self.usages {
            let span = u32::from(end.id.saturating_sub(first.id)) + 1;
            if element < skipped + span {
                return Some(Usage::new(first.page, first.id + (element - skipped) as u16));
            }
            skipped += span;
            last = Some(*end);
        }
        if self.variable { last } else { None }
    }

    pub fn element(&self, element: u32) -> Option<Location> {
        if element >= self.count {
            return None;
        }
        Some(Location {
            report: self.report,
            offset: self.offset + element * self.size,
            size: self.size,
            logical: self.logical,
            physical: self.physical,
            unit: self.unit,
            exponent: self.exponent,
        })
    }
}

impl Location {
    pub fn bytes(&self) -> usize {
        (self.offset + self.size).div_ceil(8) as usize
    }

    pub fn read(&self, payload: &[u8]) -> Option<i64> {
        if self.size == 0 || self.size > MAX_ELEMENT_BITS || payload.len() < self.bytes() {
            return None;
        }
        let mut value = 0u64;
        for bit in 0..self.size {
            let at = self.offset + bit;
            let set = payload[(at / 8) as usize] >> (at % 8) & 1;
            value |= u64::from(set) << bit;
        }
        if self.logical.min < 0 && value >> (self.size - 1) & 1 == 1 { Some(value as i64 - (1i64 << self.size)) } else { Some(value as i64) }
    }

    pub fn write(&self, payload: &mut [u8], value: i64) -> Option<()> {
        if self.size == 0 || self.size > MAX_ELEMENT_BITS || payload.len() < self.bytes() || !self.logical.contains(value) {
            return None;
        }
        let raw = value as u64;
        for bit in 0..self.size {
            let at = self.offset + bit;
            let mask = 1u8 << (at % 8);
            let byte = &mut payload[(at / 8) as usize];
            if raw >> bit & 1 == 1 {
                *byte |= mask;
            } else {
                *byte &= !mask;
            }
        }
        Some(())
    }
}

impl ReportDescriptor {
    pub fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        if bytes.len() > MAX_DESCRIPTOR {
            return Err(ParseError::TooLong(bytes.len()));
        }
        let mut out = ReportDescriptor::default();
        let mut global = Global::default();
        let mut pushed: Vec<Global> = Vec::new();
        let mut local = Local::default();
        let mut open: Vec<usize> = Vec::new();
        let mut unnumbered = false;
        let mut at = 0;
        while at < bytes.len() {
            let start = at;
            let prefix = bytes[at];
            if prefix == 0xfe {
                let size = *bytes.get(at + 1).ok_or(ParseError::Truncated(start))? as usize;
                at += 3 + size;
                if at > bytes.len() {
                    return Err(ParseError::Truncated(start));
                }
                continue;
            }
            let size = [0, 1, 2, 4][usize::from(prefix & 3)];
            let data = bytes.get(at + 1..at + 1 + size).ok_or(ParseError::Truncated(start))?;
            at += 1 + size;
            match ((prefix >> 2) & 3, prefix >> 4) {
                (0, tag @ (8 | 9 | 11)) => {
                    let kind = match tag {
                        8 => Kind::Input,
                        9 => Kind::Output,
                        _ => Kind::Feature,
                    };
                    if global.report == 0 && out.numbered {
                        return Err(ParseError::MixedNumbering(start));
                    }
                    unnumbered |= global.report == 0;
                    out.main(kind, unsigned(data), &global, &local, open.last().copied(), start)?;
                    local = Local::default();
                }
                (0, 10) => {
                    if open.len() >= MAX_DEPTH {
                        return Err(ParseError::TooDeep(start));
                    }
                    if out.collections.len() >= MAX_COLLECTIONS {
                        return Err(ParseError::TooManyCollections(start));
                    }
                    let usage = local.usages.first().map_or(Usage::new(global.page, 0), |(first, _)| *first);
                    out.collections.push(Collection { kind: unsigned(data) as u8, usage, parent: open.last().copied() });
                    open.push(out.collections.len() - 1);
                    local = Local::default();
                }
                (0, 12) => {
                    open.pop().ok_or(ParseError::UnbalancedCollection(start))?;
                    local = Local::default();
                }
                (1, 0) => {
                    let page = unsigned(data);
                    global.page = u16::try_from(page).map_err(|_| ParseError::BadPage(start))?;
                }
                (1, 1) => global.logical_min = signed(data),
                (1, 2) => global.logical_max = (i64::from(unsigned(data)), signed(data)),
                (1, 3) => global.physical_min = signed(data),
                (1, 4) => global.physical_max = (i64::from(unsigned(data)), signed(data)),
                (1, 5) => {
                    let nibble = (unsigned(data) & 0xf) as i8;
                    global.exponent = if nibble > 7 { nibble - 16 } else { nibble };
                }
                (1, 6) => global.unit = unsigned(data),
                (1, 7) => global.size = unsigned(data),
                (1, 8) => {
                    let report = unsigned(data);
                    if report == 0 || report > 255 {
                        return Err(ParseError::ZeroReportId(start));
                    }
                    if unnumbered {
                        return Err(ParseError::MixedNumbering(start));
                    }
                    global.report = report as u8;
                    out.numbered = true;
                }
                (1, 9) => global.count = unsigned(data),
                (1, 10) => {
                    if pushed.len() >= MAX_PUSHED {
                        return Err(ParseError::TooManyPushes(start));
                    }
                    pushed.push(global.clone());
                }
                (1, 11) => global = pushed.pop().ok_or(ParseError::PopWithoutPush(start))?,
                (2, 0) => {
                    if local.usages.len() >= MAX_USAGE_RANGES {
                        return Err(ParseError::TooManyUsages(start));
                    }
                    let usage = usage_of(data, global.page);
                    local.usages.push((usage, usage));
                }
                (2, 1) => local.minimum = Some(usage_of(data, global.page)),
                (2, 2) => {
                    let end = usage_of(data, global.page);
                    if let Some(first) = local.minimum.take() {
                        if local.usages.len() >= MAX_USAGE_RANGES {
                            return Err(ParseError::TooManyUsages(start));
                        }
                        if first.page == end.page && first.id <= end.id {
                            local.usages.push((first, end));
                        }
                    }
                }
                _ => {}
            }
        }
        if !open.is_empty() {
            return Err(ParseError::UnclosedCollection);
        }
        Ok(out)
    }

    fn main(&mut self, kind: Kind, flags: u32, global: &Global, local: &Local, collection: Option<usize>, start: usize) -> Result<(), ParseError> {
        let bits = global.size.checked_mul(global.count).ok_or(ParseError::ReportTooLong(start))?;
        let offset = self.grow(kind, global.report, bits).ok_or(ParseError::ReportTooLong(start))?;
        let constant = flags & 1 == 1;
        if constant || bits == 0 || local.usages.is_empty() {
            return Ok(());
        }
        let variable = flags & 2 == 2;
        if variable && global.size > MAX_ELEMENT_BITS {
            return Err(ParseError::ElementTooWide(start));
        }
        if self.fields.len() >= MAX_FIELDS {
            return Err(ParseError::TooManyFields(start));
        }
        self.fields.push(Field {
            kind,
            report: global.report,
            offset,
            size: global.size,
            count: global.count,
            variable,
            relative: flags & 4 == 4,
            usages: local.usages.clone(),
            logical: bounded(global.logical_min, global.logical_max),
            physical: bounded(global.physical_min, global.physical_max),
            unit: global.unit,
            exponent: global.exponent,
            collection,
        });
        Ok(())
    }

    fn grow(&mut self, kind: Kind, report: u8, bits: u32) -> Option<u32> {
        let at = match self.sizes.iter().position(|(k, r, _)| *k == kind && *r == report) {
            Some(at) => at,
            None => {
                self.sizes.push((kind, report, 0));
                self.sizes.len() - 1
            }
        };
        let offset = self.sizes[at].2;
        let total = offset.checked_add(bits)?;
        if total.div_ceil(8) as usize > MAX_REPORT_BYTES {
            return None;
        }
        self.sizes[at].2 = total;
        Some(offset)
    }

    pub fn payload_bytes(&self, kind: Kind, report: u8) -> Option<usize> {
        self.sizes.iter().find(|(k, r, _)| *k == kind && *r == report).map(|(_, _, bits)| bits.div_ceil(8) as usize)
    }

    pub fn application_of(&self, mut collection: usize) -> Option<usize> {
        loop {
            let node = self.collections.get(collection)?;
            if node.kind == APPLICATION {
                return Some(collection);
            }
            collection = node.parent?;
        }
    }

    pub fn within(&self, collection: Option<usize>, ancestor: usize) -> bool {
        let mut current = collection;
        while let Some(index) = current {
            if index == ancestor {
                return true;
            }
            current = self.collections.get(index).and_then(|node| node.parent);
        }
        false
    }

    pub fn find(&self, kind: Kind, usage: Usage, scope: usize) -> Option<Location> {
        self.fields
            .iter()
            .filter(|field| field.kind == kind && field.variable && self.within(field.collection, scope))
            .find_map(|field| (0..field.count).find(|element| field.usage(*element) == Some(usage)).and_then(|element| field.element(element)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ELAN: &[u8] = include_bytes!("../tests/data/elan-327e.rdesc");

    #[test]
    fn the_elan_descriptor_parses_into_its_collections_and_reports() {
        let parsed = ReportDescriptor::parse(ELAN).unwrap();
        assert!(parsed.numbered);
        let applications: Vec<Usage> = parsed.collections.iter().filter(|c| c.kind == APPLICATION).map(|c| c.usage).collect();
        assert_eq!(applications, [Usage::new(0x01, 0x02), Usage::new(0xff00, 0x01), Usage::new(0x0d, 0x05), Usage::new(0x0d, 0x0e)]);
        let fingers = parsed.collections.iter().filter(|c| c.usage == Usage::new(0x0d, 0x22) && c.kind == LOGICAL).count();
        assert_eq!(fingers, 5);
        assert_eq!(parsed.payload_bytes(Kind::Input, 0x54), Some(28));
        assert_eq!(parsed.payload_bytes(Kind::Feature, 0x0c), Some(1160));
        assert_eq!(parsed.payload_bytes(Kind::Feature, 0x03), Some(2));
    }

    #[test]
    fn bits_are_read_and_written_little_endian_with_sign_where_the_range_is_signed() {
        let location =
            Location { report: 1, offset: 4, size: 12, logical: Range { min: 0, max: 4095 }, physical: Range { min: 0, max: 0 }, unit: 0, exponent: 0 };
        let mut payload = [0u8; 2];
        location.write(&mut payload, 0xabc).unwrap();
        assert_eq!(payload, [0xc0, 0xab]);
        assert_eq!(location.read(&payload), Some(0xabc));
        assert_eq!(location.write(&mut payload, 4096), None);
        let signed = Location { logical: Range { min: -127, max: 127 }, offset: 0, size: 8, ..location };
        assert_eq!(signed.read(&[0xff]), Some(-1));
        assert_eq!(signed.read(&[]), None);
    }

    #[test]
    fn hostile_descriptors_are_refused_without_panicking() {
        let deep: Vec<u8> = core::iter::repeat_n([0xa1u8, 0x01], 20).flatten().collect();
        assert_eq!(ReportDescriptor::parse(&deep).err(), Some(ParseError::TooDeep(32)));
        assert_eq!(ReportDescriptor::parse(&[0xa1, 0x01]).err(), Some(ParseError::UnclosedCollection));
        assert_eq!(ReportDescriptor::parse(&[0xc0]).err(), Some(ParseError::UnbalancedCollection(0)));
        assert_eq!(ReportDescriptor::parse(&[0x95]).err(), Some(ParseError::Truncated(0)));
        assert_eq!(ReportDescriptor::parse(&[0xb4]).err(), Some(ParseError::PopWithoutPush(0)));
        assert_eq!(ReportDescriptor::parse(&[0x85, 0x00]).err(), Some(ParseError::ZeroReportId(0)));
        let huge = [0x09, 0x01, 0x75, 0x08, 0x96, 0xff, 0xff, 0x81, 0x02];
        assert_eq!(ReportDescriptor::parse(&huge).err(), Some(ParseError::ReportTooLong(7)));
        let wide = [0x09, 0x01, 0x77, 0x40, 0x00, 0x00, 0x00, 0x95, 0x01, 0x81, 0x02];
        assert_eq!(ReportDescriptor::parse(&wide).err(), Some(ParseError::ElementTooWide(9)));
        let mixed = [0x09, 0x01, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02, 0x85, 0x01];
        assert_eq!(ReportDescriptor::parse(&mixed).err(), Some(ParseError::MixedNumbering(8)));
        let long = [0xfe, 0x05, 0x00, 1, 2];
        assert_eq!(ReportDescriptor::parse(&long).err(), Some(ParseError::Truncated(0)));
        assert_eq!(ReportDescriptor::parse(&[0u8; MAX_DESCRIPTOR + 1]).err(), Some(ParseError::TooLong(MAX_DESCRIPTOR + 1)));
        for cut in 0..ELAN.len() {
            let _ = ReportDescriptor::parse(&ELAN[..cut]);
        }
    }

    #[test]
    fn usage_ranges_and_extended_usages_resolve_per_element() {
        let descriptor = [0x05, 0x09, 0x19, 0x01, 0x29, 0x03, 0x0b, 0x30, 0x00, 0x01, 0x00, 0x75, 0x01, 0x95, 0x05, 0x81, 0x02];
        let parsed = ReportDescriptor::parse(&descriptor).unwrap();
        let field = &parsed.fields[0];
        let usages: Vec<Option<Usage>> = (0..5).map(|e| field.usage(e)).collect();
        assert_eq!(usages, [Some(Usage::new(9, 1)), Some(Usage::new(9, 2)), Some(Usage::new(9, 3)), Some(Usage::new(1, 0x30)), Some(Usage::new(1, 0x30))]);
    }
}
