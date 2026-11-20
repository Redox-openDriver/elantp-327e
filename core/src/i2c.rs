use alloc::vec::Vec;

use crate::report::MAX_DESCRIPTOR;

pub const DESCRIPTOR_LENGTH: usize = 30;
pub const VERSION: u16 = 0x0100;
const EXTENDED_ID: u8 = 0x0f;
const MAX_INPUT: u16 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HidDescriptor {
    pub report_descriptor_length: u16,
    pub report_descriptor_register: u16,
    pub input_register: u16,
    pub max_input: u16,
    pub output_register: u16,
    pub max_output: u16,
    pub command_register: u16,
    pub data_register: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DescriptorError {
    Length(u16),
    Version(u16),
    ReportDescriptorLength(u16),
    MaxInput(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opcode {
    Reset = 1,
    GetReport = 2,
    SetReport = 3,
    SetPower = 8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportType {
    Input = 1,
    Output = 2,
    Feature = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Power {
    On = 0,
    Sleep = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input<'a> {
    Report(&'a [u8]),
    Reset,
    Malformed(u16),
}

fn word(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

impl HidDescriptor {
    pub fn parse(bytes: &[u8; DESCRIPTOR_LENGTH]) -> Result<Self, DescriptorError> {
        let length = word(bytes, 0);
        if usize::from(length) != DESCRIPTOR_LENGTH {
            return Err(DescriptorError::Length(length));
        }
        let version = word(bytes, 2);
        if version != VERSION {
            return Err(DescriptorError::Version(version));
        }
        let descriptor = HidDescriptor {
            report_descriptor_length: word(bytes, 4),
            report_descriptor_register: word(bytes, 6),
            input_register: word(bytes, 8),
            max_input: word(bytes, 10),
            output_register: word(bytes, 12),
            max_output: word(bytes, 14),
            command_register: word(bytes, 16),
            data_register: word(bytes, 18),
            vendor: word(bytes, 20),
            product: word(bytes, 22),
            version: word(bytes, 24),
        };
        if descriptor.report_descriptor_length == 0 || usize::from(descriptor.report_descriptor_length) > MAX_DESCRIPTOR {
            return Err(DescriptorError::ReportDescriptorLength(descriptor.report_descriptor_length));
        }
        if descriptor.max_input < 3 || descriptor.max_input > MAX_INPUT {
            return Err(DescriptorError::MaxInput(descriptor.max_input));
        }
        Ok(descriptor)
    }

    fn command(&self, opcode: Opcode, low: u8, report: Option<u8>) -> Vec<u8> {
        let mut out = Vec::with_capacity(16);
        out.extend_from_slice(&self.command_register.to_le_bytes());
        match report {
            Some(id) if id >= EXTENDED_ID => {
                out.push(low | EXTENDED_ID);
                out.push(opcode as u8);
                out.push(id);
            }
            Some(id) => {
                out.push(low | id);
                out.push(opcode as u8);
            }
            None => {
                out.push(low);
                out.push(opcode as u8);
            }
        }
        out
    }

    pub fn reset(&self) -> Vec<u8> {
        self.command(Opcode::Reset, 0, None)
    }

    pub fn set_power(&self, power: Power) -> Vec<u8> {
        self.command(Opcode::SetPower, power as u8, None)
    }

    pub fn get_report(&self, kind: ReportType, report: u8) -> Vec<u8> {
        let mut out = self.command(Opcode::GetReport, (kind as u8) << 4, Some(report));
        out.extend_from_slice(&self.data_register.to_le_bytes());
        out
    }

    pub fn set_report(&self, kind: ReportType, report: u8, payload: &[u8]) -> Option<Vec<u8>> {
        let length = u16::try_from(2 + 1 + payload.len()).ok()?;
        let mut out = self.command(Opcode::SetReport, (kind as u8) << 4, Some(report));
        out.extend_from_slice(&self.data_register.to_le_bytes());
        out.extend_from_slice(&length.to_le_bytes());
        out.push(report);
        out.extend_from_slice(payload);
        Some(out)
    }
}

pub fn input(buffer: &[u8]) -> Input<'_> {
    if buffer.len() < 2 {
        return Input::Malformed(0);
    }
    let length = word(buffer, 0);
    match usize::from(length) {
        0 => Input::Reset,
        n if n < 3 || n > buffer.len() => Input::Malformed(length),
        n => Input::Report(&buffer[2..n]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elan() -> HidDescriptor {
        let mut bytes = [0u8; DESCRIPTOR_LENGTH];
        for (at, value) in [30u16, 0x0100, 675, 2, 3, 31, 4, 0, 5, 6, 0x04f3, 0x327e, 0x0001].iter().enumerate() {
            bytes[at * 2..at * 2 + 2].copy_from_slice(&value.to_le_bytes());
        }
        HidDescriptor::parse(&bytes).unwrap()
    }

    #[test]
    fn the_hid_descriptor_is_read_and_checked() {
        let descriptor = elan();
        assert_eq!((descriptor.vendor, descriptor.product, descriptor.max_input, descriptor.report_descriptor_length), (0x04f3, 0x327e, 31, 675));
        let mut bytes = [0u8; DESCRIPTOR_LENGTH];
        assert_eq!(HidDescriptor::parse(&bytes), Err(DescriptorError::Length(0)));
        bytes[0] = 30;
        assert_eq!(HidDescriptor::parse(&bytes), Err(DescriptorError::Version(0)));
        bytes[2..4].copy_from_slice(&VERSION.to_le_bytes());
        assert_eq!(HidDescriptor::parse(&bytes), Err(DescriptorError::ReportDescriptorLength(0)));
        bytes[4..6].copy_from_slice(&5000u16.to_le_bytes());
        assert_eq!(HidDescriptor::parse(&bytes), Err(DescriptorError::ReportDescriptorLength(5000)));
        bytes[4..6].copy_from_slice(&675u16.to_le_bytes());
        assert_eq!(HidDescriptor::parse(&bytes), Err(DescriptorError::MaxInput(0)));
    }

    #[test]
    fn commands_are_laid_out_as_the_hid_over_i2c_specification_says() {
        let descriptor = elan();
        assert_eq!(descriptor.reset(), [0x05, 0x00, 0x00, 0x01]);
        assert_eq!(descriptor.set_power(Power::On), [0x05, 0x00, 0x00, 0x08]);
        assert_eq!(descriptor.set_power(Power::Sleep), [0x05, 0x00, 0x01, 0x08]);
        assert_eq!(descriptor.get_report(ReportType::Feature, 0x02), [0x05, 0x00, 0x32, 0x02, 0x06, 0x00]);
        assert_eq!(descriptor.set_report(ReportType::Feature, 0x03, &[3, 0]).unwrap(), [0x05, 0x00, 0x33, 0x03, 0x06, 0x00, 0x05, 0x00, 0x03, 3, 0]);
        assert_eq!(descriptor.get_report(ReportType::Feature, 0x54), [0x05, 0x00, 0x3f, 0x02, 0x54, 0x06, 0x00]);
        assert!(descriptor.set_report(ReportType::Feature, 0x03, &[0u8; 70000]).is_none());
    }

    #[test]
    fn input_reports_are_unwrapped_from_their_length_prefix() {
        assert_eq!(input(&[0x00, 0x00, 0xff]), Input::Reset);
        assert_eq!(input(&[0x05, 0x00, 0x54, 0x01, 0x02, 0x99]), Input::Report(&[0x54, 0x01, 0x02]));
        assert_eq!(input(&[0x09, 0x00, 0x54]), Input::Malformed(9));
        assert_eq!(input(&[0x02, 0x00, 0x54]), Input::Malformed(2));
        assert_eq!(input(&[0x05]), Input::Malformed(0));
    }
}
