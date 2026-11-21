use std::fs::File;
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;

pub(crate) const UI_SET_EVBIT: libc::c_ulong = 0x4004_5564;
const UI_SET_KEYBIT: libc::c_ulong = 0x4004_5565;
const UI_SET_RELBIT: libc::c_ulong = 0x4004_5566;
const UI_SET_ABSBIT: libc::c_ulong = 0x4004_5567;
const UI_SET_PROPBIT: libc::c_ulong = 0x4004_556e;
const UI_ABS_SETUP: libc::c_ulong = 0x401c_5504;
const UI_DEV_SETUP: libc::c_ulong = 0x405c_5503;
const UI_DEV_CREATE: libc::c_ulong = 0x5501;
const UI_DEV_DESTROY: libc::c_ulong = 0x5502;
pub(crate) const EV_SYN: u16 = 0;
pub(crate) const EV_KEY: u16 = 1;
const EV_REL: u16 = 2;
pub(crate) const EV_ABS: u16 = 3;
pub(crate) const ABS_X: u16 = 0;
pub(crate) const ABS_Y: u16 = 1;
const BTN_TOOL_PEN: u16 = 0x140;
pub(crate) const BTN_TOUCH: u16 = 0x14a;
pub(crate) const INPUT_PROP_POINTER: u16 = 0;
const PEN_UNITS: i32 = 10_000;
pub(crate) const SYN_REPORT: u16 = 0;
const REL_X: u16 = 0;
const REL_Y: u16 = 1;
const REL_HWHEEL: u16 = 6;
const REL_WHEEL: u16 = 8;
const REL_WHEEL_HI_RES: u16 = 11;
const REL_HWHEEL_HI_RES: u16 = 12;
const BUS_VIRTUAL: u16 = 0x06;
const NOTCH: i32 = 120;

pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_MIDDLE: u16 = 0x112;

#[repr(C)]
struct Setup {
    bus: u16,
    vendor: u16,
    product: u16,
    version: u16,
    name: [u8; 80],
    effects: u32,
}

pub struct Pointer {
    file: File,
    carry: (f32, f32),
    wheel: (f32, f32),
    notches: (i32, i32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Id {
    pub bus: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
}

impl Id {
    pub const VIRTUAL: Id = Id { bus: BUS_VIRTUAL, vendor: 0, product: 0, version: 1 };
}

pub(crate) fn control(file: &File, request: libc::c_ulong, value: libc::c_int) -> io::Result<()> {
    if unsafe { libc::ioctl(file.as_raw_fd(), request, value) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn event(kind: u16, code: u16, value: i32) -> [u8; 24] {
    let mut bytes = [0u8; 24];
    bytes[16..18].copy_from_slice(&kind.to_ne_bytes());
    bytes[18..20].copy_from_slice(&code.to_ne_bytes());
    bytes[20..24].copy_from_slice(&value.to_ne_bytes());
    bytes
}

pub fn whole(carry: &mut f32, delta: f32) -> i32 {
    let total = *carry + delta;
    let step = total.trunc();
    *carry = total - step;
    step as i32
}

pub(crate) fn create(name: &str, id: Id, setup: impl FnOnce(&File) -> io::Result<()>) -> io::Result<File> {
    let file = std::fs::OpenOptions::new().write(true).custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK).open("/dev/uinput")?;
    setup(&file)?;
    let mut device = Setup { bus: id.bus, vendor: id.vendor, product: id.product, version: id.version, name: [0; 80], effects: 0 };
    let length = name.len().min(device.name.len() - 1);
    device.name[..length].copy_from_slice(&name.as_bytes()[..length]);
    if unsafe { libc::ioctl(file.as_raw_fd(), UI_DEV_SETUP, &device) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::ioctl(file.as_raw_fd(), UI_DEV_CREATE) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}

pub(crate) fn axis(file: &File, code: u16, minimum: i32, maximum: i32, resolution: i32) -> io::Result<()> {
    control(file, UI_SET_ABSBIT, code.into())?;
    let setup = AbsSetup { code, value: minimum, minimum, maximum, fuzz: 0, flat: 0, resolution };
    if unsafe { libc::ioctl(file.as_raw_fd(), UI_ABS_SETUP, &setup) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn properties(file: &File, properties: &[u16]) -> io::Result<()> {
    for property in properties {
        control(file, UI_SET_PROPBIT, (*property).into())?;
    }
    Ok(())
}

pub(crate) fn keys(file: &File, codes: impl IntoIterator<Item = u16>) -> io::Result<()> {
    control(file, UI_SET_EVBIT, EV_KEY.into())?;
    for code in codes {
        control(file, UI_SET_KEYBIT, code.into())?;
    }
    Ok(())
}

pub(crate) fn write(file: &mut File, events: &[(u16, u16, i32)]) -> io::Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    let mut bytes = Vec::with_capacity(events.len() * 24);
    for (kind, code, value) in events {
        bytes.extend_from_slice(&event(*kind, *code, *value));
    }
    file.write_all(&bytes)
}

fn send(file: &mut File, events: &[[u8; 24]]) -> io::Result<()> {
    let mut bytes = Vec::with_capacity(events.len() * 24 + 24);
    for event in events {
        bytes.extend_from_slice(event);
    }
    bytes.extend_from_slice(&event(EV_SYN, SYN_REPORT, 0));
    file.write_all(&bytes)
}

pub(crate) fn destroy(file: &File) {
    unsafe { libc::ioctl(file.as_raw_fd(), UI_DEV_DESTROY) };
}

impl Pointer {
    pub fn create(name: &str) -> io::Result<Self> {
        let file = create(name, Id::VIRTUAL, |file| {
            keys(file, [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE])?;
            control(file, UI_SET_EVBIT, EV_REL.into())?;
            for axis in [REL_X, REL_Y, REL_WHEEL, REL_HWHEEL, REL_WHEEL_HI_RES, REL_HWHEEL_HI_RES] {
                control(file, UI_SET_RELBIT, axis.into())?;
            }
            Ok(())
        })?;
        Ok(Pointer { file, carry: (0.0, 0.0), wheel: (0.0, 0.0), notches: (0, 0) })
    }

    fn send(&mut self, events: &[[u8; 24]]) -> io::Result<()> {
        send(&mut self.file, events)
    }

    pub fn motion(&mut self, dx: f32, dy: f32) -> io::Result<()> {
        let x = whole(&mut self.carry.0, dx);
        let y = whole(&mut self.carry.1, dy);
        let mut events = Vec::with_capacity(2);
        if x != 0 {
            events.push(event(EV_REL, REL_X, x));
        }
        if y != 0 {
            events.push(event(EV_REL, REL_Y, y));
        }
        if events.is_empty() { Ok(()) } else { self.send(&events) }
    }

    pub fn button(&mut self, code: u16, pressed: bool) -> io::Result<()> {
        self.send(&[event(EV_KEY, code, i32::from(pressed))])
    }

    pub fn scroll(&mut self, dx: f32, dy: f32, units_per_notch: f32) -> io::Result<()> {
        let horizontal = whole(&mut self.wheel.0, dx * NOTCH as f32 / units_per_notch);
        let vertical = whole(&mut self.wheel.1, -dy * NOTCH as f32 / units_per_notch);
        let mut events = Vec::with_capacity(4);
        if horizontal != 0 {
            events.push(event(EV_REL, REL_HWHEEL_HI_RES, horizontal));
            self.notches.0 += horizontal;
            let steps = self.notches.0 / NOTCH;
            if steps != 0 {
                self.notches.0 -= steps * NOTCH;
                events.push(event(EV_REL, REL_HWHEEL, steps));
            }
        }
        if vertical != 0 {
            events.push(event(EV_REL, REL_WHEEL_HI_RES, vertical));
            self.notches.1 += vertical;
            let steps = self.notches.1 / NOTCH;
            if steps != 0 {
                self.notches.1 -= steps * NOTCH;
                events.push(event(EV_REL, REL_WHEEL, steps));
            }
        }
        if events.is_empty() { Ok(()) } else { self.send(&events) }
    }
}

impl Drop for Pointer {
    fn drop(&mut self) {
        destroy(&self.file);
    }
}

pub struct Keyboard {
    file: File,
}

impl Keyboard {
    pub fn create(name: &str, codes: impl IntoIterator<Item = u16>) -> io::Result<Self> {
        let file = create(name, Id::VIRTUAL, |file| keys(file, codes))?;
        Ok(Keyboard { file })
    }

    pub fn key(&mut self, code: u16, pressed: bool) -> io::Result<()> {
        send(&mut self.file, &[event(EV_KEY, code, i32::from(pressed))])
    }
}

impl Drop for Keyboard {
    fn drop(&mut self) {
        destroy(&self.file);
    }
}

#[repr(C)]
struct AbsSetup {
    code: u16,
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

pub struct Pen {
    file: File,
    near: bool,
    down: bool,
}

impl Pen {
    pub fn create(name: &str, width: f32, height: f32) -> io::Result<Self> {
        let file = create(name, Id::VIRTUAL, |file| {
            keys(file, [BTN_TOOL_PEN, BTN_TOUCH])?;
            control(file, UI_SET_EVBIT, EV_ABS.into())?;
            properties(file, &[INPUT_PROP_POINTER])?;
            for (code, millimetres) in [(ABS_X, width), (ABS_Y, height)] {
                let resolution = (PEN_UNITS as f32 / millimetres.max(1.0)).round() as i32;
                axis(file, code, 0, PEN_UNITS, resolution.max(1))?;
            }
            Ok(())
        })?;
        Ok(Pen { file, near: false, down: false })
    }

    pub fn place(&mut self, x: f32, y: f32, down: bool) -> io::Result<()> {
        let mut events = Vec::with_capacity(4);
        if !self.near {
            events.push(event(EV_KEY, BTN_TOOL_PEN, 1));
            self.near = true;
        }
        events.push(event(EV_ABS, ABS_X, (x.clamp(0.0, 1.0) * PEN_UNITS as f32).round() as i32));
        events.push(event(EV_ABS, ABS_Y, (y.clamp(0.0, 1.0) * PEN_UNITS as f32).round() as i32));
        if down != self.down {
            events.push(event(EV_KEY, BTN_TOUCH, i32::from(down)));
            self.down = down;
        }
        send(&mut self.file, &events)
    }

    pub fn away(&mut self) -> io::Result<()> {
        let mut events = Vec::with_capacity(2);
        if self.down {
            events.push(event(EV_KEY, BTN_TOUCH, 0));
            self.down = false;
        }
        if self.near {
            events.push(event(EV_KEY, BTN_TOOL_PEN, 0));
            self.near = false;
        }
        if events.is_empty() { Ok(()) } else { send(&mut self.file, &events) }
    }
}

impl Drop for Pen {
    fn drop(&mut self) {
        let _ = self.away();
        destroy(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractions_are_carried_so_slow_motion_is_not_lost() {
        let mut carry = 0.0;
        let steps: Vec<i32> = [0.4, 0.4, 0.4, -1.5, 2.7].iter().map(|d| whole(&mut carry, *d)).collect();
        assert_eq!(steps, [0, 0, 1, -1, 2]);
        assert!((carry - 0.4).abs() < 1e-5, "{carry}");
    }

    #[test]
    fn events_are_laid_out_as_the_kernel_reads_them() {
        let bytes = event(EV_REL, REL_WHEEL_HI_RES, -120);
        assert_eq!(&bytes[..16], &[0u8; 16]);
        assert_eq!(u16::from_ne_bytes([bytes[16], bytes[17]]), EV_REL);
        assert_eq!(u16::from_ne_bytes([bytes[18], bytes[19]]), REL_WHEEL_HI_RES);
        assert_eq!(i32::from_ne_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]), -120);
        assert_eq!(std::mem::size_of::<Setup>(), 92);
        assert_eq!(std::mem::size_of::<AbsSetup>(), 28);
    }
}
