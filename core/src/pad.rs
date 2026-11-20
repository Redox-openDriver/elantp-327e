use alloc::vec;
use alloc::vec::Vec;

use crate::i2c::{self, DESCRIPTOR_LENGTH, DescriptorError, HidDescriptor, Input, Power, ReportType};
use crate::identity::{self, Unverified};
use crate::report::{ParseError, ReportDescriptor};
use crate::touchpad::{Assembler, Frame, InputMode, Layout, LayoutError, Setting};

pub trait Bus {
    type Error;

    fn transfer(&mut self, write: &[u8], read: &mut [u8]) -> Result<(), Self::Error>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PadError<E> {
    Bus(E),
    Descriptor(DescriptorError),
    Unverified(Unverified),
    Report(ParseError),
    Layout(LayoutError),
    Setting,
    Malformed(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Resetting { since: u64 },
    Running,
}

pub struct Pad<B> {
    bus: B,
    pub descriptor: HidDescriptor,
    pub layout: Layout,
    assembler: Assembler,
    input: Vec<u8>,
    pub state: State,
}

pub const RESET_PATIENCE: u64 = 1_000_000;

impl<B: Bus> Pad<B> {
    pub fn start(mut bus: B, register: u16, now: u64) -> Result<Self, PadError<B::Error>> {
        let mut raw = [0u8; DESCRIPTOR_LENGTH];
        bus.transfer(&register.to_le_bytes(), &mut raw).map_err(PadError::Bus)?;
        let descriptor = HidDescriptor::parse(&raw).map_err(PadError::Descriptor)?;
        let mut report = vec![0u8; usize::from(descriptor.report_descriptor_length)];
        bus.transfer(&descriptor.report_descriptor_register.to_le_bytes(), &mut report).map_err(PadError::Bus)?;
        identity::verify(descriptor.vendor, descriptor.product, &report).map_err(PadError::Unverified)?;
        let parsed = ReportDescriptor::parse(&report).map_err(PadError::Report)?;
        let layout = Layout::from_descriptor(&parsed).map_err(PadError::Layout)?;
        bus.transfer(&descriptor.set_power(Power::On), &mut []).map_err(PadError::Bus)?;
        bus.transfer(&descriptor.reset(), &mut []).map_err(PadError::Bus)?;
        let input = vec![0u8; usize::from(descriptor.max_input)];
        Ok(Pad { bus, descriptor, layout, assembler: Assembler::default(), input, state: State::Resetting { since: now } })
    }

    fn configure(&mut self) -> Result<(), PadError<B::Error>> {
        for setting in [Setting::InputMode(InputMode::Touchpad), Setting::Reporting { surface: true, button: true }] {
            let (report, payload) = self.layout.feature(setting).ok_or(PadError::Setting)?;
            let command = self.descriptor.set_report(ReportType::Feature, report, &payload).ok_or(PadError::Setting)?;
            self.bus.transfer(&command, &mut []).map_err(PadError::Bus)?;
        }
        self.state = State::Running;
        Ok(())
    }

    pub fn tick(&mut self, now: u64) -> Result<(), PadError<B::Error>> {
        match self.state {
            State::Resetting { since } if now.saturating_sub(since) >= RESET_PATIENCE => self.configure(),
            _ => Ok(()),
        }
    }

    pub fn interrupt(&mut self) -> Result<Option<Frame>, PadError<B::Error>> {
        self.bus.transfer(&[], &mut self.input).map_err(PadError::Bus)?;
        let decoded = match i2c::input(&self.input) {
            Input::Reset => None,
            Input::Malformed(length) => return Err(PadError::Malformed(length)),
            Input::Report(bytes) => Some(self.layout.decode(bytes)),
        };
        if matches!(self.state, State::Resetting { .. }) {
            self.configure()?;
        }
        match decoded {
            Some(Ok(report)) => Ok(self.assembler.push(report)),
            Some(Err(_)) | None => Ok(None),
        }
    }

    pub fn sleep(&mut self) -> Result<(), PadError<B::Error>> {
        self.bus.transfer(&self.descriptor.set_power(Power::Sleep), &mut []).map_err(PadError::Bus)
    }
}

#[cfg(test)]
mod tests {
    use alloc::collections::VecDeque;

    use super::*;

    const ELAN: &[u8] = include_bytes!("../tests/data/elan-327e.rdesc");

    fn hid_descriptor(product: u16) -> [u8; DESCRIPTOR_LENGTH] {
        let mut bytes = [0u8; DESCRIPTOR_LENGTH];
        for (at, value) in [30u16, 0x0100, ELAN.len() as u16, 2, 3, 31, 4, 0, 5, 6, 0x04f3, product, 0x0001].iter().enumerate() {
            bytes[at * 2..at * 2 + 2].copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    struct Elan {
        product: u16,
        writes: Vec<Vec<u8>>,
        inputs: VecDeque<Vec<u8>>,
    }

    impl Bus for Elan {
        type Error = ();

        fn transfer(&mut self, write: &[u8], read: &mut [u8]) -> Result<(), ()> {
            match write {
                [0x01, 0x00] => read.copy_from_slice(&hid_descriptor(self.product)),
                [0x02, 0x00] => read.copy_from_slice(ELAN),
                [] => {
                    let input = self.inputs.pop_front().ok_or(())?;
                    read.fill(0);
                    read[..input.len()].copy_from_slice(&input);
                }
                other => self.writes.push(other.to_vec()),
            }
            Ok(())
        }
    }

    fn elan(inputs: Vec<Vec<u8>>) -> Elan {
        Elan { product: 0x327e, writes: Vec::new(), inputs: inputs.into() }
    }

    fn report(contacts: &[(u8, u16, u16)]) -> Vec<u8> {
        let mut bytes = vec![31, 0, 0x54, (contacts.len() as u8) << 4, 0x10, 0x00];
        for slot in 0..5 {
            match contacts.get(slot) {
                Some(&(id, x, y)) => {
                    bytes.push(0b11 | id << 4);
                    bytes.extend_from_slice(&x.to_le_bytes());
                    bytes.extend_from_slice(&y.to_le_bytes());
                }
                None => bytes.extend_from_slice(&[0; 5]),
            }
        }
        bytes
    }

    #[test]
    fn starting_powers_the_pad_resets_it_and_after_the_reset_switches_it_to_touchpad_mode() {
        let mut pad = Pad::start(elan(vec![vec![0, 0], report(&[(1, 1000, 500)])]), 0x0001, 0).unwrap();
        assert_eq!(pad.state, State::Resetting { since: 0 });
        assert_eq!(pad.bus.writes, [vec![0x05, 0x00, 0x00, 0x08], vec![0x05, 0x00, 0x00, 0x01]]);
        assert_eq!(pad.interrupt().unwrap(), None);
        assert_eq!(pad.state, State::Running);
        assert_eq!(pad.bus.writes[2], [0x05, 0x00, 0x33, 0x03, 0x06, 0x00, 0x05, 0x00, 0x03, 0x03, 0x00]);
        assert_eq!(pad.bus.writes[3], [0x05, 0x00, 0x35, 0x03, 0x06, 0x00, 0x05, 0x00, 0x05, 0x03, 0x00]);
        let frame = pad.interrupt().unwrap().unwrap();
        assert_eq!((frame.contacts.len(), frame.contacts[0].x, frame.contacts[0].y), (1, 1000, 500));
    }

    #[test]
    fn a_pad_that_never_answers_the_reset_is_configured_after_a_second() {
        let mut pad = Pad::start(elan(vec![]), 0x0001, 5).unwrap();
        pad.tick(500_000).unwrap();
        assert_eq!(pad.state, State::Resetting { since: 5 });
        pad.tick(1_000_005).unwrap();
        assert_eq!(pad.state, State::Running);
        assert_eq!(pad.bus.writes.len(), 4);
    }

    #[test]
    fn another_pad_is_refused_before_anything_is_sent_to_it() {
        let mut other = elan(vec![]);
        other.product = 0x3282;
        assert_eq!(Pad::start(other, 0x0001, 0).err(), Some(PadError::Unverified(Unverified::Product(0x3282))));
    }

    #[test]
    fn a_broken_length_prefix_is_an_error_not_a_frame() {
        let mut pad = Pad::start(elan(vec![vec![0, 0], vec![0xff, 0x7f]]), 0x0001, 0).unwrap();
        pad.interrupt().unwrap();
        assert_eq!(pad.interrupt().err(), Some(PadError::Malformed(0x7fff)));
    }
}
