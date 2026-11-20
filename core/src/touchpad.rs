use alloc::vec::Vec;

use crate::report::{APPLICATION, Kind, LOGICAL, Location, ReportDescriptor, Usage};

pub const TOUCH_PAD: Usage = Usage::new(0x0d, 0x05);
pub const CONFIGURATION: Usage = Usage::new(0x0d, 0x0e);
pub const FINGER: Usage = Usage::new(0x0d, 0x22);
pub const TIP_SWITCH: Usage = Usage::new(0x0d, 0x42);
pub const CONFIDENCE: Usage = Usage::new(0x0d, 0x47);
pub const CONTACT_ID: Usage = Usage::new(0x0d, 0x51);
pub const INPUT_MODE: Usage = Usage::new(0x0d, 0x52);
pub const CONTACT_COUNT: Usage = Usage::new(0x0d, 0x54);
pub const CONTACT_COUNT_MAXIMUM: Usage = Usage::new(0x0d, 0x55);
pub const SCAN_TIME: Usage = Usage::new(0x0d, 0x56);
pub const SURFACE_SWITCH: Usage = Usage::new(0x0d, 0x57);
pub const BUTTON_SWITCH: Usage = Usage::new(0x0d, 0x58);
pub const PAD_TYPE: Usage = Usage::new(0x0d, 0x59);
pub const LATENCY_MODE: Usage = Usage::new(0x0d, 0x60);
pub const X: Usage = Usage::new(0x01, 0x30);
pub const Y: Usage = Usage::new(0x01, 0x31);
pub const BUTTON_1: Usage = Usage::new(0x09, 0x01);

pub const MAX_SLOTS: usize = 16;
const UNIT_CENTIMETRE: u32 = 0x11;
const UNIT_INCH: u32 = 0x13;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutError {
    NoTouchPad,
    NoFingers,
    TooManySlots(usize),
    Missing(Usage),
    SplitReport,
    UnevenSlots,
    BadAxis(Usage),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    pub tip: Location,
    pub confidence: Option<Location>,
    pub id: Location,
    pub x: Location,
    pub y: Location,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Axis {
    pub logical_min: i64,
    pub logical_max: i64,
    pub micrometres: i64,
}

impl Axis {
    fn of(location: &Location, usage: Usage) -> Result<Self, LayoutError> {
        let span = location.physical.max - location.physical.min;
        let scale = match location.unit {
            UNIT_CENTIMETRE => 10_000i64,
            UNIT_INCH => 25_400i64,
            _ => return Err(LayoutError::BadAxis(usage)),
        };
        let mut micrometres = span.checked_mul(scale).ok_or(LayoutError::BadAxis(usage))?;
        let exponent = i32::from(location.exponent);
        for _ in 0..exponent.unsigned_abs() {
            micrometres = if exponent < 0 { micrometres / 10 } else { micrometres.checked_mul(10).ok_or(LayoutError::BadAxis(usage))? };
        }
        if location.logical.max <= location.logical.min || micrometres <= 0 {
            return Err(LayoutError::BadAxis(usage));
        }
        Ok(Axis { logical_min: location.logical.min, logical_max: location.logical.max, micrometres })
    }

    pub fn units_per_millimetre(&self) -> f32 {
        (self.logical_max - self.logical_min) as f32 * 1000.0 / self.micrometres as f32
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub report: u8,
    pub length: usize,
    pub slots: Vec<Slot>,
    pub count: Location,
    pub scan_time: Location,
    pub button: Option<Location>,
    pub x: Axis,
    pub y: Axis,
    pub maximum: Option<Location>,
    pub pad_type: Option<Location>,
    pub latency: Option<Location>,
    pub input_mode: Option<Location>,
    pub surface_switch: Option<Location>,
    pub button_switch: Option<Location>,
    feature_lengths: Vec<(u8, usize)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Contact {
    pub id: u8,
    pub x: i64,
    pub y: i64,
    pub tip: bool,
    pub confident: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub count: u8,
    pub scan_time: u16,
    pub button: bool,
    pub slots: Vec<Contact>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub scan_time: u16,
    pub button: bool,
    pub contacts: Vec<Contact>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    OtherReport(u8),
    Short { expected: usize, got: usize },
    BadCount(i64),
    BadContact(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Mouse = 0,
    Touchpad = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    InputMode(InputMode),
    Reporting { surface: bool, button: bool },
    Latency { high: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadType {
    Clickpad,
    Pressure,
    NonClickable,
    Other(i64),
}

fn need(location: Option<Location>, usage: Usage) -> Result<Location, LayoutError> {
    location.ok_or(LayoutError::Missing(usage))
}

impl Layout {
    pub fn from_descriptor(descriptor: &ReportDescriptor) -> Result<Self, LayoutError> {
        let pad = descriptor.collections.iter().position(|c| c.kind == APPLICATION && c.usage == TOUCH_PAD).ok_or(LayoutError::NoTouchPad)?;
        let fingers: Vec<usize> = (0..descriptor.collections.len())
            .filter(|&i| {
                let node = &descriptor.collections[i];
                node.kind == LOGICAL && node.usage == FINGER && descriptor.within(node.parent, pad)
            })
            .collect();
        if fingers.is_empty() {
            return Err(LayoutError::NoFingers);
        }
        if fingers.len() > MAX_SLOTS {
            return Err(LayoutError::TooManySlots(fingers.len()));
        }
        let input = |usage: Usage, scope: usize| descriptor.find(Kind::Input, usage, scope);
        let mut slots = Vec::with_capacity(fingers.len());
        for &finger in &fingers {
            slots.push(Slot {
                tip: need(input(TIP_SWITCH, finger), TIP_SWITCH)?,
                confidence: input(CONFIDENCE, finger),
                id: need(input(CONTACT_ID, finger), CONTACT_ID)?,
                x: need(input(X, finger), X)?,
                y: need(input(Y, finger), Y)?,
            });
        }
        let count = need(input(CONTACT_COUNT, pad), CONTACT_COUNT)?;
        let scan_time = need(input(SCAN_TIME, pad), SCAN_TIME)?;
        let button = input(BUTTON_1, pad);
        let report = count.report;
        let mut locations = slots.iter().flat_map(|slot| [Some(slot.tip), slot.confidence, Some(slot.id), Some(slot.x), Some(slot.y)]).flatten();
        if scan_time.report != report || button.is_some_and(|b| b.report != report) || locations.any(|l| l.report != report) {
            return Err(LayoutError::SplitReport);
        }
        let first = slots[0];
        let same = |a: &Location, b: &Location| a.logical == b.logical && a.physical == b.physical && a.unit == b.unit && a.exponent == b.exponent;
        if slots.iter().any(|slot| !same(&slot.x, &first.x) || !same(&slot.y, &first.y)) {
            return Err(LayoutError::UnevenSlots);
        }
        let length = descriptor.payload_bytes(Kind::Input, report).unwrap_or(0);
        let configuration = descriptor.collections.iter().position(|c| c.kind == APPLICATION && c.usage == CONFIGURATION);
        let feature = |usage: Usage, scope: Option<usize>| scope.and_then(|scope| descriptor.find(Kind::Feature, usage, scope));
        let mut layout = Layout {
            report,
            length,
            x: Axis::of(&first.x, X)?,
            y: Axis::of(&first.y, Y)?,
            slots,
            count,
            scan_time,
            button,
            maximum: feature(CONTACT_COUNT_MAXIMUM, Some(pad)),
            pad_type: feature(PAD_TYPE, Some(pad)),
            latency: feature(LATENCY_MODE, Some(pad)),
            input_mode: feature(INPUT_MODE, configuration),
            surface_switch: feature(SURFACE_SWITCH, configuration),
            button_switch: feature(BUTTON_SWITCH, configuration),
            feature_lengths: Vec::new(),
        };
        let reports: Vec<u8> = [layout.maximum, layout.pad_type, layout.latency, layout.input_mode, layout.surface_switch, layout.button_switch]
            .into_iter()
            .flatten()
            .map(|l| l.report)
            .collect();
        for id in reports {
            if !layout.feature_lengths.iter().any(|(r, _)| *r == id) {
                let bytes = descriptor.payload_bytes(Kind::Feature, id).unwrap_or(0);
                layout.feature_lengths.push((id, bytes));
            }
        }
        Ok(layout)
    }

    pub fn decode(&self, report: &[u8]) -> Result<Report, DecodeError> {
        let (&id, payload) = report.split_first().ok_or(DecodeError::Short { expected: self.length + 1, got: 0 })?;
        if id != self.report {
            return Err(DecodeError::OtherReport(id));
        }
        if payload.len() < self.length {
            return Err(DecodeError::Short { expected: self.length + 1, got: report.len() });
        }
        let read = |location: &Location| location.read(payload);
        let count = read(&self.count).ok_or(DecodeError::Short { expected: self.length + 1, got: report.len() })?;
        if !(0..=i64::from(u8::MAX)).contains(&count) {
            return Err(DecodeError::BadCount(count));
        }
        let scan_time = read(&self.scan_time).unwrap_or(0) as u16;
        let button = self.button.and_then(|b| read(&b)).is_some_and(|v| v != 0);
        let mut slots = Vec::with_capacity(self.slots.len());
        for (index, slot) in self.slots.iter().enumerate() {
            let value = |location: &Location| read(location).ok_or(DecodeError::BadContact(index));
            let id = value(&slot.id)?;
            let contact = Contact {
                id: u8::try_from(id).map_err(|_| DecodeError::BadContact(index))?,
                x: value(&slot.x)?,
                y: value(&slot.y)?,
                tip: value(&slot.tip)? != 0,
                confident: slot.confidence.map_or(Ok(true), |c| value(&c).map(|v| v != 0))?,
            };
            slots.push(contact);
        }
        Ok(Report { count: count as u8, scan_time, button, slots })
    }

    pub fn feature(&self, setting: Setting) -> Option<(u8, Vec<u8>)> {
        let writes: Vec<(Location, i64)> = match setting {
            Setting::InputMode(mode) => alloc::vec![(self.input_mode?, mode as i64)],
            Setting::Reporting { surface, button } => {
                let mut writes = alloc::vec![(self.surface_switch?, i64::from(surface))];
                if let Some(switch) = self.button_switch {
                    writes.push((switch, i64::from(button)));
                }
                writes
            }
            Setting::Latency { high } => alloc::vec![(self.latency?, i64::from(high))],
        };
        let report = writes[0].0.report;
        if writes.iter().any(|(location, _)| location.report != report) {
            return None;
        }
        let length = self.feature_lengths.iter().find(|(r, _)| *r == report)?.1;
        let mut payload = alloc::vec![0u8; length];
        for (location, value) in writes {
            location.write(&mut payload, value)?;
        }
        Some((report, payload))
    }

    pub fn feature_length(&self, report: u8) -> Option<usize> {
        self.feature_lengths.iter().find(|(r, _)| *r == report).map(|(_, length)| *length)
    }

    pub fn maximum_contacts(&self, feature: &[u8]) -> Option<u8> {
        let location = self.maximum?;
        let (&id, payload) = feature.split_first()?;
        (id == location.report).then(|| location.read(payload)).flatten().and_then(|v| u8::try_from(v).ok())
    }

    pub fn pad_type(&self, feature: &[u8]) -> Option<PadType> {
        let location = self.pad_type?;
        let (&id, payload) = feature.split_first()?;
        if id != location.report {
            return None;
        }
        Some(match location.read(payload)? {
            0 => PadType::Clickpad,
            1 => PadType::Pressure,
            2 => PadType::NonClickable,
            other => PadType::Other(other),
        })
    }
}

#[derive(Debug, Default)]
pub struct Assembler {
    pending: Option<(Frame, usize)>,
}

impl Assembler {
    pub fn push(&mut self, report: Report) -> Option<Frame> {
        let Report { count, scan_time, button, slots } = report;
        let (mut frame, wanted) = if count > 0 {
            (Frame { scan_time, button, contacts: Vec::with_capacity(usize::from(count)) }, usize::from(count))
        } else {
            match self.pending.take() {
                Some((pending, wanted)) if pending.scan_time == scan_time => (pending, wanted),
                Some(_) => return None,
                None => return Some(Frame { scan_time, button, contacts: Vec::new() }),
            }
        };
        self.pending = None;
        frame.button |= button;
        let missing = wanted - frame.contacts.len();
        frame.contacts.extend(slots.into_iter().take(missing));
        if frame.contacts.len() >= wanted {
            Some(frame)
        } else {
            self.pending = Some((frame, wanted));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ELAN: &[u8] = include_bytes!("../tests/data/elan-327e.rdesc");

    fn layout() -> Layout {
        Layout::from_descriptor(&ReportDescriptor::parse(ELAN).unwrap()).unwrap()
    }

    fn report(layout: &Layout, button: bool, scan_time: u16, count: u8, contacts: &[(u8, i64, i64, bool, bool)]) -> Vec<u8> {
        let mut payload = alloc::vec![0u8; layout.length];
        layout.count.write(&mut payload, i64::from(count)).unwrap();
        layout.scan_time.write(&mut payload, i64::from(scan_time)).unwrap();
        if let Some(location) = layout.button {
            location.write(&mut payload, i64::from(button)).unwrap();
        }
        for (slot, &(id, x, y, tip, confident)) in layout.slots.iter().zip(contacts) {
            slot.id.write(&mut payload, i64::from(id)).unwrap();
            slot.x.write(&mut payload, x).unwrap();
            slot.y.write(&mut payload, y).unwrap();
            slot.tip.write(&mut payload, i64::from(tip)).unwrap();
            slot.confidence.unwrap().write(&mut payload, i64::from(confident)).unwrap();
        }
        let mut out = alloc::vec![layout.report];
        out.extend(payload);
        out
    }

    #[test]
    fn the_elan_pad_is_a_five_finger_clickpad_of_117_by_72_millimetres() {
        let layout = layout();
        assert_eq!((layout.report, layout.length, layout.slots.len()), (0x54, 28, 5));
        assert_eq!((layout.x.logical_max, layout.x.micrometres), (3679, 117_000));
        assert_eq!((layout.y.logical_max, layout.y.micrometres), (2261, 72_000));
        assert!((layout.x.units_per_millimetre() - 31.44).abs() < 0.01);
        assert!(layout.button.is_some() && layout.maximum.is_some() && layout.pad_type.is_some() && layout.latency.is_some());
        assert_eq!(layout.input_mode.map(|l| l.report), Some(0x03));
        assert_eq!(layout.surface_switch.map(|l| l.report), Some(0x05));
    }

    #[test]
    fn a_report_decodes_into_every_slot_and_the_assembler_keeps_the_counted_ones() {
        let layout = layout();
        let bytes = report(&layout, true, 0x1234, 2, &[(3, 1000, 500, true, true), (7, 3679, 2261, false, false), (9, 1, 1, true, true)]);
        assert_eq!(bytes.len(), 29);
        let decoded = layout.decode(&bytes).unwrap();
        assert_eq!((decoded.count, decoded.scan_time, decoded.button, decoded.slots.len()), (2, 0x1234, true, 5));
        let frame = Assembler::default().push(decoded).unwrap();
        assert_eq!(
            frame.contacts,
            [Contact { id: 3, x: 1000, y: 500, tip: true, confident: true }, Contact { id: 7, x: 3679, y: 2261, tip: false, confident: false }]
        );
        assert_eq!(layout.decode(&bytes[..20]).err(), Some(DecodeError::Short { expected: 29, got: 20 }));
        assert_eq!(layout.decode(&[0x01, 0, 0, 0]).err(), Some(DecodeError::OtherReport(0x01)));
        assert_eq!(layout.decode(&[]).err(), Some(DecodeError::Short { expected: 29, got: 0 }));
    }

    #[test]
    fn settings_become_the_feature_reports_the_pad_describes_and_nothing_else() {
        let layout = layout();
        assert_eq!(layout.feature(Setting::InputMode(InputMode::Touchpad)), Some((0x03, alloc::vec![3, 0])));
        assert_eq!(layout.feature(Setting::InputMode(InputMode::Mouse)), Some((0x03, alloc::vec![0, 0])));
        assert_eq!(layout.feature(Setting::Reporting { surface: true, button: true }), Some((0x05, alloc::vec![0b11, 0])));
        assert_eq!(layout.feature(Setting::Reporting { surface: true, button: false }), Some((0x05, alloc::vec![0b01, 0])));
        assert_eq!(layout.feature(Setting::Latency { high: false }), Some((0x07, alloc::vec![0, 0])));
        assert_eq!(layout.maximum_contacts(&[0x02, 0x05]), Some(5));
        assert_eq!(layout.pad_type(&[0x02, 0x05]), Some(PadType::Clickpad));
        assert_eq!(layout.maximum_contacts(&[0x06, 0x05]), None);
    }

    #[test]
    fn contacts_split_across_reports_are_put_back_together() {
        let layout = layout();
        let decode = |button, scan, count, contacts: &[(u8, i64, i64, bool, bool)]| layout.decode(&report(&layout, button, scan, count, contacts)).unwrap();
        let first = decode(false, 10, 7, &[(0, 1, 1, true, true), (1, 1, 1, true, true), (2, 1, 1, true, true), (3, 1, 1, true, true), (4, 1, 1, true, true)]);
        let rest = decode(true, 10, 0, &[(5, 2, 2, true, true), (6, 3, 3, true, true), (9, 9, 9, true, true)]);
        let mut assembler = Assembler::default();
        assert_eq!(assembler.push(first.clone()), None);
        let whole = assembler.push(rest.clone()).unwrap();
        assert_eq!(whole.contacts.iter().map(|c| c.id).collect::<Vec<_>>(), [0, 1, 2, 3, 4, 5, 6]);
        assert!(whole.button);
        assert_eq!(assembler.push(first.clone()), None);
        let mut late = rest;
        late.scan_time = 11;
        assert_eq!(assembler.push(late), None);
        let single = decode(false, 12, 1, &[(1, 5, 5, true, true)]);
        assert_eq!(assembler.push(single).map(|f| f.contacts.len()), Some(1));
        let lifted = decode(false, 13, 0, &[]);
        assert_eq!(assembler.push(lifted), Some(Frame { scan_time: 13, button: false, contacts: Vec::new() }));
    }

    #[test]
    fn a_descriptor_without_a_touch_pad_or_with_broken_axes_is_refused() {
        let mouse = [0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x09, 0x30, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06, 0xc0];
        assert_eq!(Layout::from_descriptor(&ReportDescriptor::parse(&mouse).unwrap()).err(), Some(LayoutError::NoTouchPad));
        let mut broken = ELAN.to_vec();
        let units: Vec<usize> = broken.windows(3).enumerate().filter(|(_, w)| *w == [0x65, 0x11, 0x75]).map(|(at, _)| at).collect();
        assert_eq!(units.len(), 5);
        for at in units {
            broken[at + 1] = 0x14;
        }
        assert_eq!(Layout::from_descriptor(&ReportDescriptor::parse(&broken).unwrap()).err(), Some(LayoutError::BadAxis(X)));
    }
}
