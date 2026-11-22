use std::collections::BTreeSet;
use std::io;
use std::os::fd::RawFd;
use std::path::PathBuf;

use crate::evdev::bit;
use crate::watch::{self, Watched};

const EV_KEY: usize = 1;
const LETTERS: [usize; 3] = [30, 44, 57];
const MODIFIERS: [u16; 9] = [29, 97, 42, 54, 56, 100, 125, 126, 464];

pub fn is_keyboard(events: &str, keys: &str) -> bool {
    bit(events, EV_KEY) && LETTERS.iter().all(|key| bit(keys, *key))
}

pub fn find() -> io::Result<Vec<PathBuf>> {
    watch::nodes(|device| is_keyboard(device.events, device.keys))
}

#[derive(Default)]
pub struct Keyboards {
    watched: Watched,
    held: BTreeSet<u16>,
}

impl Keyboards {
    pub fn refresh(&mut self) {
        if let Ok(nodes) = find() {
            self.watched.refresh(&nodes);
        }
    }

    pub fn clear(&mut self) {
        self.watched.clear();
        self.held.clear();
    }

    pub fn count(&self) -> usize {
        self.watched.count()
    }

    pub fn fds(&self) -> Vec<RawFd> {
        self.watched.fds()
    }

    pub fn typed(&mut self) -> bool {
        let (held, mut typed) = (&mut self.held, false);
        self.watched.events(|kind, code, value| typed |= press(held, kind, code, value));
        typed
    }
}

fn press(held: &mut BTreeSet<u16>, kind: u16, code: u16, value: i32) -> bool {
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
        assert!(press(&mut held, 1, 30, 1));
        assert!(!press(&mut held, 1, 30, 2));
        assert!(!press(&mut held, 1, 30, 0));
        assert!(!press(&mut held, 1, 29, 1));
        assert!(!press(&mut held, 1, 46, 1));
        assert!(!press(&mut held, 1, 29, 0));
        assert!(press(&mut held, 1, 46, 1));
        assert!(!press(&mut held, 2, 0, 5));
    }
}
