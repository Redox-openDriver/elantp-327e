use std::io;
use std::os::fd::RawFd;
use std::path::PathBuf;

use crate::evdev::bit;
use crate::watch::{self, Watched};

pub const TOUCHPAD_TOGGLE: u16 = 0x212;
pub const TOUCHPAD_ON: u16 = 0x213;
pub const TOUCHPAD_OFF: u16 = 0x214;
pub const ANNOUNCE_ON: u16 = 192;
pub const ANNOUNCE_OFF: u16 = 193;
const EV_KEY: u16 = 1;
const PRESSED: i32 = 1;

pub fn is_switch(events: &str, keys: &str) -> bool {
    bit(events, usize::from(EV_KEY)) && [TOUCHPAD_TOGGLE, TOUCHPAD_ON, TOUCHPAD_OFF].iter().any(|code| bit(keys, usize::from(*code)))
}

pub fn find() -> io::Result<Vec<PathBuf>> {
    watch::nodes(|device| is_switch(device.events, device.keys))
}

pub fn switch(kind: u16, code: u16, value: i32) -> Option<Option<bool>> {
    if kind != EV_KEY || value != PRESSED {
        return None;
    }
    match code {
        TOUCHPAD_TOGGLE => Some(None),
        TOUCHPAD_ON => Some(Some(true)),
        TOUCHPAD_OFF => Some(Some(false)),
        _ => None,
    }
}

pub fn announcement(on: bool) -> u16 {
    if on { ANNOUNCE_ON } else { ANNOUNCE_OFF }
}

#[derive(Default)]
pub struct Switches {
    watched: Watched,
}

impl Switches {
    pub fn refresh(&mut self) {
        if let Ok(nodes) = find() {
            self.watched.refresh(&nodes);
        }
    }

    pub fn count(&self) -> usize {
        self.watched.count()
    }

    pub fn fds(&self) -> Vec<RawFd> {
        self.watched.fds()
    }

    pub fn pressed(&mut self) -> Vec<Option<bool>> {
        let mut pressed = Vec::new();
        self.watched.events(|kind, code, value| pressed.extend(switch(kind, code, value)));
        pressed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_devices_that_can_send_a_touchpad_key_are_heard_and_only_its_presses_count() {
        assert!(is_switch("13", "400040000000000 40000001c0000 0 6000000000000000 100000000000 0 181000800100c00 400000000300400 0 2"));
        assert!(!is_switch("120013", "402000007 ff803078f800d001 feffffdfffcfffff fffffffffffffffe"));
        assert!(!is_switch("1b", "e520 10000 0 0 0 0"));
        assert!(!is_switch("", ""));
        assert_eq!(switch(1, TOUCHPAD_TOGGLE, 1), Some(None));
        assert_eq!(switch(1, TOUCHPAD_ON, 1), Some(Some(true)));
        assert_eq!(switch(1, TOUCHPAD_OFF, 1), Some(Some(false)));
        assert_eq!(switch(1, TOUCHPAD_TOGGLE, 0), None);
        assert_eq!(switch(1, TOUCHPAD_TOGGLE, 2), None);
        assert_eq!(switch(1, 30, 1), None);
        assert_eq!(switch(4, TOUCHPAD_TOGGLE, 1), None);
        assert_eq!((announcement(true), announcement(false)), (192, 193));
    }
}
