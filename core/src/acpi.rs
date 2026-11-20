use alloc::string::String;
use alloc::vec::Vec;

pub const HID_DSM_UUID: [u8; 16] = [0xf7, 0xf6, 0xdf, 0x3c, 0x67, 0x42, 0x55, 0x45, 0xad, 0x05, 0xb3, 0x0a, 0x3d, 0x89, 0x38, 0xde];
pub const HID_DSM_REVISION: u64 = 1;
pub const HID_DSM_DESCRIPTOR_ADDRESS: u64 = 1;

const END_TAG: u8 = 0x0f;
const IRQ: u8 = 0x04;
const MEMORY32_FIXED: u8 = 0x06;
const EXTENDED_INTERRUPT: u8 = 0x09;
const GPIO_CONNECTION: u8 = 0x0c;
const SERIAL_BUS: u8 = 0x0e;
const SERIAL_BUS_I2C: u8 = 1;
const GPIO_INTERRUPT: u8 = 0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resource {
    Memory { base: u32, length: u32, writable: bool },
    Interrupt { numbers: Vec<u32>, edge: bool, active_low: bool, shared: bool },
    GpioInterrupt { pins: Vec<u16>, edge: bool, active_low: bool, both: bool, shared: bool, debounce: u16, controller: String },
    I2c { address: u16, speed: u32, ten_bit: bool, controller: String },
    Other(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceError {
    Truncated(usize),
    MissingEnd,
    BadLength(usize),
    BadOffset(usize),
}

fn word(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]))
}

fn dword(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes([*bytes.get(at)?, *bytes.get(at + 1)?, *bytes.get(at + 2)?, *bytes.get(at + 3)?]))
}

fn source(body: &[u8], at: usize, end: usize) -> Option<String> {
    let name = body.get(at..end)?;
    let name = name.split(|b| *b == 0).next()?;
    name.iter().all(|b| b.is_ascii_graphic()).then(|| name.iter().map(|b| char::from(*b)).collect())
}

fn gpio(body: &[u8], start: usize) -> Result<Resource, ResourceError> {
    let at = |offset: usize| word(body, offset).map(usize::from).ok_or(ResourceError::Truncated(start));
    let kind = *body.get(1).ok_or(ResourceError::Truncated(start))?;
    if kind != GPIO_INTERRUPT {
        return Ok(Resource::Other(GPIO_CONNECTION));
    }
    let flags = word(body, 4).ok_or(ResourceError::Truncated(start))?;
    let debounce = word(body, 9).ok_or(ResourceError::Truncated(start))?;
    let (pins_at, name_at, vendor_at) = (at(11)?, at(14)?, at(16)?);
    let pins_at = pins_at.checked_sub(3).ok_or(ResourceError::BadOffset(start))?;
    let name_at = name_at.checked_sub(3).ok_or(ResourceError::BadOffset(start))?;
    let vendor_at = vendor_at.checked_sub(3).ok_or(ResourceError::BadOffset(start))?;
    if !(pins_at <= name_at && name_at <= vendor_at && vendor_at <= body.len()) || (name_at - pins_at) % 2 != 0 {
        return Err(ResourceError::BadOffset(start));
    }
    let pins = body[pins_at..name_at].as_chunks::<2>().0.iter().map(|pin| u16::from_le_bytes(*pin)).collect();
    let controller = source(body, name_at, vendor_at).ok_or(ResourceError::BadOffset(start))?;
    let polarity = (flags >> 1) & 3;
    Ok(Resource::GpioInterrupt {
        pins,
        edge: flags & 1 == 1,
        active_low: polarity == 1,
        both: polarity == 2,
        shared: flags >> 3 & 1 == 1,
        debounce,
        controller,
    })
}

fn serial(body: &[u8], start: usize) -> Result<Resource, ResourceError> {
    let kind = *body.get(2).ok_or(ResourceError::Truncated(start))?;
    if kind != SERIAL_BUS_I2C {
        return Ok(Resource::Other(SERIAL_BUS));
    }
    let specific = word(body, 4).ok_or(ResourceError::Truncated(start))?;
    let data_length = usize::from(word(body, 7).ok_or(ResourceError::Truncated(start))?);
    let speed = dword(body, 9).ok_or(ResourceError::Truncated(start))?;
    let address = word(body, 13).ok_or(ResourceError::Truncated(start))?;
    let name_at = 9 + data_length;
    if name_at > body.len() {
        return Err(ResourceError::BadOffset(start));
    }
    let controller = source(body, name_at, body.len()).ok_or(ResourceError::BadOffset(start))?;
    Ok(Resource::I2c { address, speed, ten_bit: specific & 1 == 1, controller })
}

pub fn resources(template: &[u8]) -> Result<Vec<Resource>, ResourceError> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < template.len() {
        let start = at;
        let tag = template[at];
        if tag & 0x80 == 0 {
            let length = usize::from(tag & 7);
            let name = (tag >> 3) & 0x0f;
            if name == END_TAG {
                return Ok(out);
            }
            let body = template.get(at + 1..at + 1 + length).ok_or(ResourceError::Truncated(start))?;
            at += 1 + length;
            out.push(match (name, body) {
                (IRQ, [low, high, rest @ ..]) => {
                    let mask = u16::from_le_bytes([*low, *high]);
                    let flags = rest.first().copied().unwrap_or(1);
                    Resource::Interrupt {
                        numbers: (0..16).filter(|bit| mask >> bit & 1 == 1).collect(),
                        edge: flags & 1 == 1,
                        active_low: flags >> 3 & 1 == 1,
                        shared: flags >> 4 & 1 == 1,
                    }
                }
                (IRQ, _) => return Err(ResourceError::BadLength(start)),
                _ => Resource::Other(name),
            });
            continue;
        }
        let length = usize::from(word(template, at + 1).ok_or(ResourceError::Truncated(start))?);
        let body = template.get(at + 3..at + 3 + length).ok_or(ResourceError::Truncated(start))?;
        at += 3 + length;
        let resource = match tag & 0x7f {
            MEMORY32_FIXED => {
                if length != 9 {
                    return Err(ResourceError::BadLength(start));
                }
                Resource::Memory { base: dword(body, 1).unwrap_or(0), length: dword(body, 5).unwrap_or(0), writable: body[0] & 1 == 1 }
            }
            EXTENDED_INTERRUPT => {
                let flags = *body.first().ok_or(ResourceError::Truncated(start))?;
                let count = usize::from(*body.get(1).ok_or(ResourceError::Truncated(start))?);
                let numbers: Option<Vec<u32>> = (0..count).map(|i| dword(body, 2 + 4 * i)).collect();
                Resource::Interrupt {
                    numbers: numbers.ok_or(ResourceError::BadLength(start))?,
                    edge: flags >> 1 & 1 == 1,
                    active_low: flags >> 2 & 1 == 1,
                    shared: flags >> 3 & 1 == 1,
                }
            }
            GPIO_CONNECTION => gpio(body, start)?,
            SERIAL_BUS => serial(body, start)?,
            other => Resource::Other(other),
        };
        out.push(resource);
    }
    Err(ResourceError::MissingEnd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    fn large(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![0x80 | tag];
        out.extend_from_slice(&(body.len() as u16).to_le_bytes());
        out.extend_from_slice(body);
        out
    }

    fn i2c(address: u16, speed: u32, controller: &str) -> Vec<u8> {
        i2c_flags(address, speed, controller, 0)
    }

    fn i2c_flags(address: u16, speed: u32, controller: &str, specific: u8) -> Vec<u8> {
        let mut body = vec![2, 0, SERIAL_BUS_I2C, 0x02, specific, 0x00, 0x01, 6, 0];
        body.extend_from_slice(&speed.to_le_bytes());
        body.extend_from_slice(&address.to_le_bytes());
        body.extend_from_slice(controller.as_bytes());
        body.push(0);
        large(SERIAL_BUS, &body)
    }

    fn gpio_int(pin: u16, edge: bool, active_low: bool, controller: &str) -> Vec<u8> {
        let flags: u16 = u16::from(edge) | if active_low { 1 << 1 } else { 0 };
        let pins_at = 3 + 20u16;
        let name_at = pins_at + 2;
        let vendor_at = name_at + controller.len() as u16 + 1;
        let mut body = vec![1, GPIO_INTERRUPT];
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&flags.to_le_bytes());
        body.push(0);
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&pins_at.to_le_bytes());
        body.push(0);
        body.extend_from_slice(&name_at.to_le_bytes());
        body.extend_from_slice(&vendor_at.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&pin.to_le_bytes());
        body.extend_from_slice(controller.as_bytes());
        body.push(0);
        large(GPIO_CONNECTION, &body)
    }

    #[test]
    fn a_touchpad_template_gives_its_bus_address_and_interrupt_pin() {
        let mut template = i2c(0x15, 400_000, "\\_SB.I2CA");
        template.extend(gpio_int(9, false, true, "\\_SB.GPIO"));
        template.extend([0x79, 0x00]);
        assert_eq!(
            resources(&template).unwrap(),
            [
                Resource::I2c { address: 0x15, speed: 400_000, ten_bit: false, controller: "\\_SB.I2CA".to_string() },
                Resource::GpioInterrupt {
                    pins: vec![9],
                    edge: false,
                    active_low: true,
                    both: false,
                    shared: false,
                    debounce: 0,
                    controller: "\\_SB.GPIO".to_string()
                }
            ]
        );
    }

    #[test]
    fn ten_bit_addressing_is_read_from_the_type_specific_flags() {
        let template = [i2c_flags(0x2a5, 1_000_000, "\\_SB.I2CB", 1), vec![0x79, 0x00]].concat();
        assert_eq!(resources(&template).unwrap(), [Resource::I2c { address: 0x2a5, speed: 1_000_000, ten_bit: true, controller: "\\_SB.I2CB".to_string() }]);
    }

    #[test]
    fn a_controller_template_gives_its_registers_and_interrupt() {
        let mut template = large(MEMORY32_FIXED, &[1, 0x00, 0x20, 0xdc, 0xfe, 0x00, 0x10, 0x00, 0x00]);
        template.extend(large(EXTENDED_INTERRUPT, &[0b0001_0101, 1, 10, 0, 0, 0]));
        template.extend([0x79, 0x00]);
        assert_eq!(
            resources(&template).unwrap(),
            [
                Resource::Memory { base: 0xfedc_2000, length: 0x1000, writable: true },
                Resource::Interrupt { numbers: vec![10], edge: false, active_low: true, shared: false }
            ]
        );
    }

    #[test]
    fn the_laptops_own_templates_give_the_touchpad_and_its_controllers() {
        let touchpad = [
            0x8e, 0x19, 0x00, 0x02, 0x00, 0x01, 0x02, 0x00, 0x00, 0x01, 0x06, 0x00, 0x80, 0x1a, 0x06, 0x00, 0x15, 0x00, 0x5c, 0x5f, 0x53, 0x42, 0x2e, 0x49,
            0x32, 0x43, 0x41, 0x00, 0x8c, 0x20, 0x00, 0x01, 0x00, 0x01, 0x00, 0x12, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x17, 0x00, 0x00, 0x19, 0x00, 0x23,
            0x00, 0x00, 0x00, 0x09, 0x00, 0x5c, 0x5f, 0x53, 0x42, 0x2e, 0x47, 0x50, 0x49, 0x4f, 0x00, 0x79, 0x00,
        ];
        assert_eq!(
            resources(&touchpad).unwrap(),
            [
                Resource::I2c { address: 0x15, speed: 400_000, ten_bit: false, controller: "\\_SB.I2CA".to_string() },
                Resource::GpioInterrupt {
                    pins: vec![9],
                    edge: false,
                    active_low: true,
                    both: false,
                    shared: false,
                    debounce: 0,
                    controller: "\\_SB.GPIO".to_string()
                }
            ]
        );
        let controller = [0x23, 0x00, 0x04, 0x21, 0x86, 0x09, 0x00, 0x01, 0x00, 0x20, 0xdc, 0xfe, 0x00, 0x10, 0x00, 0x00, 0x79, 0x00];
        assert_eq!(
            resources(&controller).unwrap(),
            [
                Resource::Interrupt { numbers: vec![10], edge: true, active_low: false, shared: false },
                Resource::Memory { base: 0xfedc_2000, length: 0x1000, writable: true }
            ]
        );
        let gpio = [
            0x89, 0x06, 0x00, 0x0d, 0x01, 0x07, 0x00, 0x00, 0x00, 0x86, 0x09, 0x00, 0x01, 0x00, 0x15, 0xd8, 0xfe, 0x00, 0x04, 0x00, 0x00, 0x86, 0x09, 0x00,
            0x01, 0x00, 0x12, 0xd8, 0xfe, 0x00, 0x01, 0x00, 0x00, 0x79, 0x00,
        ];
        assert_eq!(
            resources(&gpio).unwrap(),
            [
                Resource::Interrupt { numbers: vec![7], edge: false, active_low: true, shared: true },
                Resource::Memory { base: 0xfed8_1500, length: 0x400, writable: true },
                Resource::Memory { base: 0xfed8_1200, length: 0x100, writable: true }
            ]
        );
        assert_eq!(
            resources(&[0x22, 0x00, 0x04, 0x79, 0x00]).unwrap(),
            [Resource::Interrupt { numbers: vec![10], edge: true, active_low: false, shared: false }]
        );
        assert_eq!(resources(&[0x21, 0x00, 0x79, 0x00]), Err(ResourceError::BadLength(0)));
    }

    #[test]
    fn broken_templates_are_refused_without_panicking() {
        let template = i2c(0x15, 400_000, "\\_SB.I2CA");
        assert_eq!(resources(&template), Err(ResourceError::MissingEnd));
        assert_eq!(resources(&template[..10]), Err(ResourceError::Truncated(0)));
        assert_eq!(resources(&large(MEMORY32_FIXED, &[0; 5])), Err(ResourceError::BadLength(0)));
        let mut bad = gpio_int(9, true, false, "\\_SB.GPIO");
        bad[3 + 11] = 0;
        assert_eq!(resources(&bad), Err(ResourceError::BadOffset(0)));
        let whole = [i2c(0x15, 400_000, "\\_SB.I2CA"), gpio_int(9, false, true, "\\_SB.GPIO"), vec![0x79, 0]].concat();
        for cut in 0..whole.len() {
            let _ = resources(&whole[..cut]);
        }
    }
}
