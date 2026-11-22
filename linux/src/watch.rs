use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const SYSFS: &str = "/sys/class/input";
const EVENT: usize = 24;
const BATCH: usize = 64;
const OURS: &str = "elantp";

pub struct Capabilities<'a> {
    pub name: &'a str,
    pub events: &'a str,
    pub keys: &'a str,
}

pub fn nodes(wanted: impl Fn(&Capabilities) -> bool) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(SYSFS)? {
        let entry = entry?;
        let file = entry.file_name();
        let Some(event) = file.to_str().filter(|name| name.starts_with("event")) else { continue };
        let device = entry.path().join("device");
        let read = |name: &str| std::fs::read_to_string(device.join(name)).unwrap_or_default();
        let name = read("name");
        let name = name.trim();
        if !name.starts_with(OURS) && wanted(&Capabilities { name, events: &read("capabilities/ev"), keys: &read("capabilities/key") }) {
            found.push(Path::new("/dev/input").join(event));
        }
    }
    found.sort();
    Ok(found)
}

#[derive(Default)]
pub struct Watched {
    open: Vec<(PathBuf, File)>,
}

impl Watched {
    pub fn refresh(&mut self, nodes: &[PathBuf]) {
        self.open.retain(|(path, _)| nodes.contains(path));
        for node in nodes {
            if self.open.iter().any(|(path, _)| path == node) {
                continue;
            }
            if let Ok(file) = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK).open(node) {
                self.open.push((node.clone(), file));
            }
        }
    }

    pub fn clear(&mut self) {
        self.open.clear();
    }

    pub fn count(&self) -> usize {
        self.open.len()
    }

    pub fn fds(&self) -> Vec<RawFd> {
        self.open.iter().map(|(_, file)| file.as_raw_fd()).collect()
    }

    pub fn events(&mut self, mut each: impl FnMut(u16, u16, i32)) {
        let mut gone = Vec::new();
        let mut buffer = [0u8; EVENT * BATCH];
        for (index, (_, file)) in self.open.iter().enumerate() {
            loop {
                let count = unsafe { libc::read(file.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
                if count <= 0 {
                    let error = io::Error::last_os_error();
                    if count < 0 && !matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) {
                        gone.push(index);
                    }
                    break;
                }
                let (events, _) = buffer[..count as usize].as_chunks::<EVENT>();
                for event in events {
                    let kind = u16::from_ne_bytes([event[16], event[17]]);
                    let code = u16::from_ne_bytes([event[18], event[19]]);
                    let value = i32::from_ne_bytes([event[20], event[21], event[22], event[23]]);
                    each(kind, code, value);
                }
            }
        }
        for index in gone.into_iter().rev() {
            self.open.remove(index);
        }
    }
}
