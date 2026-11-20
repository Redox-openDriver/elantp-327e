use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const SYSFS: &str = "/sys/class/input";
const EVIOCGRAB: libc::c_ulong = 0x4004_4590;

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
}
