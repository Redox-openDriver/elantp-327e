use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

const SYSFS: &str = "/sys/class/hidraw";
const BUS_I2C: u16 = 0x18;
pub const MAX_REPORT: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub node: PathBuf,
    pub name: String,
    pub bus: u16,
    pub vendor: u16,
    pub product: u16,
    pub descriptor: Vec<u8>,
}

pub fn parse_id(value: &str) -> Option<(u16, u16, u16)> {
    let mut parts = value.trim().split(':');
    let bus = u16::from_str_radix(parts.next()?, 16).ok()?;
    let vendor = u32::from_str_radix(parts.next()?, 16).ok()?;
    let product = u32::from_str_radix(parts.next()?, 16).ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((bus, u16::try_from(vendor).ok()?, u16::try_from(product).ok()?))
}

fn describe(entry: &Path) -> Option<Found> {
    let uevent = std::fs::read_to_string(entry.join("device/uevent")).ok()?;
    let field = |name: &str| uevent.lines().find_map(|line| line.strip_prefix(name).and_then(|rest| rest.strip_prefix('=')));
    let (bus, vendor, product) = parse_id(field("HID_ID")?)?;
    let descriptor = std::fs::read(entry.join("device/report_descriptor")).ok()?;
    let node = Path::new("/dev").join(entry.file_name()?);
    Some(Found { node, name: field("HID_NAME").unwrap_or_default().to_string(), bus, vendor, product, descriptor })
}

pub fn find(vendor: u16, product: u16) -> io::Result<Option<Found>> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(SYSFS)?.filter_map(|entry| entry.ok().map(|e| e.path())).collect();
    entries.sort();
    Ok(entries.iter().filter_map(|entry| describe(entry)).find(|found| found.bus == BUS_I2C && found.vendor == vendor && found.product == product))
}

pub struct Device {
    file: File,
}

const fn hidiocgfeature(length: usize) -> libc::c_ulong {
    (3 << 30) | ((length as libc::c_ulong) << 16) | (0x48 << 8) | 0x07
}

impl Device {
    pub fn open(node: &Path) -> io::Result<Self> {
        let file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK).open(node)?;
        Ok(Device { file })
    }

    pub fn wait(&self, timeout: Duration) -> io::Result<bool> {
        let mut poll = libc::pollfd { fd: self.file.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        let millis = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
        let ready = unsafe { libc::poll(&mut poll, 1, millis) };
        match ready {
            -1 => {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted { Ok(false) } else { Err(error) }
            }
            0 => Ok(false),
            _ if poll.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 => Err(io::Error::from(io::ErrorKind::BrokenPipe)),
            _ => Ok(true),
        }
    }

    pub fn read(&self, buffer: &mut [u8]) -> io::Result<Option<usize>> {
        let count = unsafe { libc::read(self.file.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
        if count >= 0 {
            return Ok(Some(count as usize));
        }
        let error = io::Error::last_os_error();
        match error.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted => Ok(None),
            _ => Err(error),
        }
    }

    pub fn feature(&self, report: u8, length: usize) -> io::Result<Vec<u8>> {
        let mut buffer = vec![0u8; length.clamp(1, MAX_REPORT)];
        buffer[0] = report;
        let count = unsafe { libc::ioctl(self.file.as_raw_fd(), hidiocgfeature(buffer.len()), buffer.as_mut_ptr()) };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        buffer.truncate(count as usize);
        Ok(buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hid_ids_from_uevent_are_read_as_bus_vendor_and_product() {
        assert_eq!(parse_id("0018:000004F3:0000327E"), Some((0x18, 0x04f3, 0x327e)));
        assert_eq!(parse_id("0003:0000046D:0000C548\n"), Some((0x03, 0x046d, 0xc548)));
        assert_eq!(parse_id("0018:000104F3:0000327E"), None);
        assert_eq!(parse_id("0018:04F3"), None);
        assert_eq!(parse_id("0018:04F3:327E:1"), None);
        assert_eq!(hidiocgfeature(3), 0xc003_4807);
    }
}
