use std::collections::BTreeSet;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::evdev::bit;

const SYSFS: &str = "/sys/class/input";
const EV_KEY: usize = 1;
const EVENT: usize = 24;
const BATCH: usize = 64;
const LETTERS: [usize; 3] = [30, 44, 57];
const MODIFIERS: [u16; 9] = [29, 97, 42, 54, 56, 100, 125, 126, 464];
const OURS: &str = "elantp";

pub fn is_keyboard(events: &str, keys: &str) -> bool {
    bit(events, EV_KEY) && LETTERS.iter().all(|key| bit(keys, *key))
}

pub fn find() -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(SYSFS)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|name| name.starts_with("event")) else { continue };
        let device = entry.path().join("device");
        let read = |file: &str| std::fs::read_to_string(device.join(file)).unwrap_or_default();
        if !read("name").trim().starts_with(OURS) && is_keyboard(&read("capabilities/ev"), &read("capabilities/key")) {
            found.push(Path::new("/dev/input").join(name));
        }
    }
    found.sort();
    Ok(found)
}

#[derive(Default)]
pub struct Keyboards {
    open: Vec<(PathBuf, File)>,
    held: BTreeSet<u16>,
}

impl Keyboards {
    pub fn refresh(&mut self) {
        let Ok(nodes) = find() else { return };
        self.open.retain(|(path, _)| nodes.contains(path));
        for node in nodes {
            if self.open.iter().any(|(path, _)| *path == node) {
                continue;
            }
            if let Ok(file) = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK).open(&node) {
                self.open.push((node, file));
            }
        }
    }

    pub fn clear(&mut self) {
        self.open.clear();
        self.held.clear();
    }

    pub fn count(&self) -> usize {
        self.open.len()
    }

    pub fn fds(&self) -> Vec<RawFd> {
        self.open.iter().map(|(_, file)| file.as_raw_fd()).collect()
    }

    pub fn typed(&mut self) -> bool {
        let mut typed = false;
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
                    typed |= press(&mut self.held, event);
                }
            }
        }
        for index in gone.into_iter().rev() {
            self.open.remove(index);
        }
        typed
    }
}

fn press(held: &mut BTreeSet<u16>, event: &[u8; EVENT]) -> bool {
    let kind = u16::from_ne_bytes([event[16], event[17]]);
    let code = u16::from_ne_bytes([event[18], event[19]]);
    let value = i32::from_ne_bytes([event[20], event[21], event[22], event[23]]);
    if usize::from(kind) != EV_KEY {
        return false;
    }
    if MODIFIERS.contains(&code) {
        if value == 0 {
            held.remove(&code);
        } else {
            held.insert(code);
        }
        return false;
    }
    value == 1 && held.is_empty()
}

pub fn wait(fds: &[RawFd], timeout: std::time::Duration) -> io::Result<Vec<i16>> {
    let mut polls: Vec<libc::pollfd> = fds.iter().map(|fd| libc::pollfd { fd: *fd, events: libc::POLLIN, revents: 0 }).collect();
    let millis = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
    let ready = unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as libc::nfds_t, millis) };
    if ready < 0 {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::Interrupted { Ok(vec![0; fds.len()]) } else { Err(error) };
    }
    Ok(polls.iter().map(|poll| poll.revents).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uinput::event;

    #[test]
    fn keyboards_are_told_apart_from_buttons_touchpads_and_media_keys() {
        assert!(is_keyboard("120013", "402000007 ff803078f800d001 feffffdfffcfffff fffffffffffffffe"));
        assert!(is_keyboard("120013", "1000000000007 ff9f207ac14057ff febeffdfffefffff fffffffffffffffe"));
        assert!(!is_keyboard("3", "8000 10000000000000 0"));
        assert!(!is_keyboard("1b", "e520 10000 0 0 0 0"));
        assert!(!is_keyboard("1f", "3f00733fff 0 0 483ffff17aff32d bfd4444600000000 1 130ff38b17d000 677bfad9415fed 19ed68000004400 10000002"));
        assert!(!is_keyboard("13", "400040000000000 40000001c0000 0 6000000000000000 100000000000 0 181000800100c00 400000000300400 0 2"));
        assert!(!is_keyboard("", ""));
    }

    #[test]
    fn letters_count_as_typing_but_modifiers_and_shortcuts_do_not() {
        let mut held = BTreeSet::new();
        assert!(press(&mut held, &event(1, 30, 1)));
        assert!(!press(&mut held, &event(1, 30, 2)));
        assert!(!press(&mut held, &event(1, 30, 0)));
        assert!(!press(&mut held, &event(1, 29, 1)));
        assert!(!press(&mut held, &event(1, 46, 1)));
        assert!(!press(&mut held, &event(1, 29, 0)));
        assert!(press(&mut held, &event(1, 46, 1)));
        assert!(!press(&mut held, &event(2, 0, 5)));
    }
}
