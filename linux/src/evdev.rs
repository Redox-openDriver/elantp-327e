use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const SYSFS: &str = "/sys/class/input";
const EVIOCGRAB: libc::c_ulong = 0x4004_4590;
const ABS_MT_POSITION_X: usize = 0x35;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub bus: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
}

pub fn bit(map: &str, index: usize) -> bool {
    let words: Option<Vec<u64>> = map.split_whitespace().rev().map(|word| u64::from_str_radix(word, 16).ok()).collect();
    words.and_then(|words| words.get(index / 64).copied()).is_some_and(|word| word >> (index % 64) & 1 == 1)
}

fn sysfs(node: &Path) -> Option<PathBuf> {
    Some(Path::new(SYSFS).join(node.file_name()?).join("device"))
}

pub fn identity(node: &Path) -> Option<Identity> {
    let device = sysfs(node)?;
    let read = |file: &str| std::fs::read_to_string(device.join(file)).ok();
    let hex = |file: &str| read(file).and_then(|value| u16::from_str_radix(value.trim(), 16).ok());
    Some(Identity {
        name: read("name")?.trim().to_string(),
        bus: hex("id/bustype")?,
        vendor: hex("id/vendor")?,
        product: hex("id/product")?,
        version: hex("id/version")?,
    })
}

pub fn multitouch(node: &Path) -> bool {
    sysfs(node).and_then(|device| std::fs::read_to_string(device.join("capabilities/abs")).ok()).is_some_and(|abs| bit(&abs, ABS_MT_POSITION_X))
}

pub fn hid_prefix(bus: u16, vendor: u16, product: u16) -> String {
    format!("{bus:04X}:{vendor:04X}:{product:04X}.")
}

pub fn nodes(prefix: &str) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(SYSFS)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|n| n.starts_with("event")) else { continue };
        let Ok(device) = std::fs::canonicalize(entry.path().join("device")) else { continue };
        if device.components().any(|part| part.as_os_str().to_str().is_some_and(|p| p.starts_with(prefix))) {
            found.push(Path::new("/dev/input").join(name));
        }
    }
    found.sort();
    Ok(found)
}

pub struct Grab {
    _held: Vec<File>,
}

pub fn grab(nodes: &[PathBuf]) -> io::Result<Grab> {
    let mut held = Vec::with_capacity(nodes.len());
    for node in nodes {
        let file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK).open(node)?;
        if unsafe { libc::ioctl(file.as_raw_fd(), EVIOCGRAB, 1 as libc::c_int) } != 0 {
            return Err(io::Error::last_os_error());
        }
        held.push(file);
    }
    Ok(Grab { _held: held })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hid_prefix_matches_the_sysfs_name_of_the_device() {
        assert_eq!(hid_prefix(0x18, 0x04f3, 0x327e), "0018:04F3:327E.");
    }

    #[test]
    fn capability_bitmaps_are_read_word_by_word_from_the_right() {
        let touchpad = "2e0800000000003";
        assert!(bit(touchpad, 0) && bit(touchpad, 1) && bit(touchpad, 0x2f) && bit(touchpad, ABS_MT_POSITION_X) && bit(touchpad, 0x39));
        assert!(!bit(touchpad, 2) && !bit(touchpad, 0x38));
        let keys = "e520 10000 0 0 0 0";
        assert!(bit(keys, 0x110) && bit(keys, 0x145) && bit(keys, 0x14a) && !bit(keys, 0x111));
        assert!(!bit("", 0) && !bit("zz", 0) && !bit("1", 64));
    }
}
