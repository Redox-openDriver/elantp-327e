use std::fs::File;
use std::io;

use elantp_core::touchpad::Axis;
use gestus::Touch;

use crate::evdev::Identity;
use crate::uinput::{self, ABS_X, ABS_Y, BTN_LEFT, BTN_TOUCH, EV_ABS, EV_KEY, EV_SYN, INPUT_PROP_POINTER, Id, SYN_REPORT};

pub const SYNTHETIC: u8 = 200;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TOOL_TYPE: u16 = 0x37;
const ABS_MT_TRACKING_ID: u16 = 0x39;
const MT_TOOL_FINGER: i32 = 0;
const MT_TOOL_PALM: i32 = 2;
const BTN_TOOL_FINGER: u16 = 0x145;
const BTN_TOOL_QUINTTAP: u16 = 0x148;
const BTN_TOOL_DOUBLETAP: u16 = 0x14d;
const BTN_TOOL_TRIPLETAP: u16 = 0x14e;
const BTN_TOOL_QUADTAP: u16 = 0x14f;
const TOOLS: [u16; 5] = [BTN_TOOL_FINGER, BTN_TOOL_DOUBLETAP, BTN_TOOL_TRIPLETAP, BTN_TOOL_QUADTAP, BTN_TOOL_QUINTTAP];
const INPUT_PROP_BUTTONPAD: u16 = 2;
const TRACKING_MAX: i32 = 0xffff;
const SPREAD: f32 = 15.0;

pub type Wire = (u16, u16, i32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Contact {
    pub id: u8,
    pub x: i32,
    pub y: i32,
    pub palm: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoder {
    slots: Vec<Option<Contact>>,
    current: Option<usize>,
    tracking: i32,
    tool: Option<u16>,
    touch: bool,
    button: bool,
    pointer: Option<(i32, i32)>,
}

impl Encoder {
    pub fn new(slots: usize) -> Self {
        Encoder { slots: vec![None; slots.max(1)], current: None, tracking: 0, tool: None, touch: false, button: false, pointer: None }
    }

    fn select(&mut self, index: usize, out: &mut Vec<Wire>) {
        if self.current != Some(index) {
            out.push((EV_ABS, ABS_MT_SLOT, index as i32));
            self.current = Some(index);
        }
    }

    pub fn frame(&mut self, contacts: &[Contact], button: bool) -> Vec<Wire> {
        let mut out = Vec::new();
        for index in 0..self.slots.len() {
            if let Some(slot) = self.slots[index]
                && !contacts.iter().any(|contact| contact.id == slot.id)
            {
                self.select(index, &mut out);
                out.push((EV_ABS, ABS_MT_TRACKING_ID, -1));
                self.slots[index] = None;
            }
        }
        for contact in contacts {
            let tool = if contact.palm { MT_TOOL_PALM } else { MT_TOOL_FINGER };
            match self.slots.iter().position(|slot| slot.is_some_and(|slot| slot.id == contact.id)) {
                Some(index) => {
                    let Some(slot) = self.slots[index] else { continue };
                    if slot == *contact {
                        continue;
                    }
                    self.select(index, &mut out);
                    if slot.palm != contact.palm {
                        out.push((EV_ABS, ABS_MT_TOOL_TYPE, tool));
                    }
                    if slot.x != contact.x {
                        out.push((EV_ABS, ABS_MT_POSITION_X, contact.x));
                    }
                    if slot.y != contact.y {
                        out.push((EV_ABS, ABS_MT_POSITION_Y, contact.y));
                    }
                    self.slots[index] = Some(*contact);
                }
                None => {
                    let Some(index) = self.slots.iter().position(Option::is_none) else { continue };
                    self.select(index, &mut out);
                    self.tracking = (self.tracking + 1) & TRACKING_MAX;
                    out.extend([
                        (EV_ABS, ABS_MT_TRACKING_ID, self.tracking),
                        (EV_ABS, ABS_MT_TOOL_TYPE, tool),
                        (EV_ABS, ABS_MT_POSITION_X, contact.x),
                        (EV_ABS, ABS_MT_POSITION_Y, contact.y),
                    ]);
                    self.slots[index] = Some(*contact);
                }
            }
        }
        let fingers: Vec<Contact> = self.slots.iter().flatten().copied().filter(|contact| !contact.palm).collect();
        if let Some(first) = fingers.first() {
            if self.pointer.map(|pointer| pointer.0) != Some(first.x) {
                out.push((EV_ABS, ABS_X, first.x));
            }
            if self.pointer.map(|pointer| pointer.1) != Some(first.y) {
                out.push((EV_ABS, ABS_Y, first.y));
            }
            self.pointer = Some((first.x, first.y));
        }
        let touch = !fingers.is_empty();
        if touch != self.touch {
            out.push((EV_KEY, BTN_TOUCH, i32::from(touch)));
            self.touch = touch;
        }
        let tool = fingers.len().checked_sub(1).map(|index| TOOLS[index.min(TOOLS.len() - 1)]);
        if tool != self.tool {
            if let Some(old) = self.tool {
                out.push((EV_KEY, old, 0));
            }
            if let Some(new) = tool {
                out.push((EV_KEY, new, 1));
            }
            self.tool = tool;
        }
        if button != self.button {
            out.push((EV_KEY, BTN_LEFT, i32::from(button)));
            self.button = button;
        }
        if !out.is_empty() {
            out.push((EV_SYN, SYN_REPORT, 0));
        }
        out
    }

    pub fn tap(&mut self, base: &[Contact], fingers: u8, x: i32, y: i32, spread: i32) -> Vec<Wire> {
        let button = self.button;
        let mut touched = base.to_vec();
        touched.extend((0..fingers.max(1)).map(|i| Contact { id: SYNTHETIC.saturating_add(i), x: x + i32::from(i) * spread, y, palm: false }));
        let mut out = self.frame(&touched, button);
        out.extend(self.frame(base, button));
        out
    }
}

fn logical(axis: &Axis, millimetres: f32) -> i32 {
    let span = (axis.logical_max - axis.logical_min) as f32;
    let value = axis.logical_min as f32 + millimetres * axis.units_per_millimetre();
    value.round().clamp(axis.logical_min as f32, axis.logical_min as f32 + span) as i32
}

pub struct Touchpad {
    file: File,
    encoder: Encoder,
    x: Axis,
    y: Axis,
}

impl Touchpad {
    pub fn create(identity: &Identity, x: &Axis, y: &Axis, slots: usize) -> io::Result<Self> {
        let id = Id { bus: identity.bus, vendor: identity.vendor, product: identity.product, version: identity.version };
        let file = uinput::create(&identity.name, id, |file| {
            uinput::keys(file, [BTN_LEFT, BTN_TOUCH, BTN_TOOL_FINGER, BTN_TOOL_DOUBLETAP, BTN_TOOL_TRIPLETAP, BTN_TOOL_QUADTAP, BTN_TOOL_QUINTTAP])?;
            uinput::control(file, uinput::UI_SET_EVBIT, EV_ABS.into())?;
            uinput::properties(file, &[INPUT_PROP_POINTER, INPUT_PROP_BUTTONPAD])?;
            for (codes, axis) in [([ABS_X, ABS_MT_POSITION_X], x), ([ABS_Y, ABS_MT_POSITION_Y], y)] {
                let resolution = axis.units_per_millimetre().round().max(1.0) as i32;
                for code in codes {
                    uinput::axis(file, code, axis.logical_min as i32, axis.logical_max as i32, resolution)?;
                }
            }
            uinput::axis(file, ABS_MT_SLOT, 0, slots.max(1) as i32 - 1, 0)?;
            uinput::axis(file, ABS_MT_TOOL_TYPE, MT_TOOL_FINGER, MT_TOOL_PALM, 0)?;
            uinput::axis(file, ABS_MT_TRACKING_ID, 0, TRACKING_MAX, 0)
        })?;
        Ok(Touchpad { file, encoder: Encoder::new(slots), x: *x, y: *y })
    }

    fn contacts(&self, touches: &[Touch]) -> Vec<Contact> {
        touches.iter().map(|touch| Contact { id: touch.id, x: logical(&self.x, touch.x), y: logical(&self.y, touch.y), palm: touch.palm }).collect()
    }

    pub fn show(&mut self, touches: &[Touch], button: bool) -> io::Result<()> {
        let events = self.encoder.frame(&self.contacts(touches), button);
        uinput::write(&mut self.file, &events)
    }

    pub fn tap(&mut self, touches: &[Touch], fingers: u8, x: f32, y: f32) -> io::Result<()> {
        let spread = (SPREAD * self.x.units_per_millimetre()).round() as i32;
        let events = self.encoder.tap(&self.contacts(touches), fingers, logical(&self.x, x), logical(&self.y, y), spread);
        uinput::write(&mut self.file, &events)
    }
}

impl Drop for Touchpad {
    fn drop(&mut self) {
        let lifted = self.encoder.frame(&[], false);
        let _ = uinput::write(&mut self.file, &lifted);
        uinput::destroy(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(id: u8, x: i32, y: i32) -> Contact {
        Contact { id, x, y, palm: false }
    }

    #[test]
    fn touches_become_slots_with_tracking_ids_finger_counts_and_a_button() {
        let mut encoder = Encoder::new(5);
        assert_eq!(
            encoder.frame(&[at(3, 100, 200)], false),
            [
                (EV_ABS, ABS_MT_SLOT, 0),
                (EV_ABS, ABS_MT_TRACKING_ID, 1),
                (EV_ABS, ABS_MT_TOOL_TYPE, MT_TOOL_FINGER),
                (EV_ABS, ABS_MT_POSITION_X, 100),
                (EV_ABS, ABS_MT_POSITION_Y, 200),
                (EV_ABS, ABS_X, 100),
                (EV_ABS, ABS_Y, 200),
                (EV_KEY, BTN_TOUCH, 1),
                (EV_KEY, BTN_TOOL_FINGER, 1),
                (EV_SYN, SYN_REPORT, 0),
            ]
        );
        assert_eq!(encoder.frame(&[at(3, 110, 200)], false), [(EV_ABS, ABS_MT_POSITION_X, 110), (EV_ABS, ABS_X, 110), (EV_SYN, SYN_REPORT, 0)]);
        assert!(encoder.frame(&[at(3, 110, 200)], false).is_empty());
        assert_eq!(
            encoder.frame(&[at(3, 110, 200), at(5, 400, 300)], true),
            [
                (EV_ABS, ABS_MT_SLOT, 1),
                (EV_ABS, ABS_MT_TRACKING_ID, 2),
                (EV_ABS, ABS_MT_TOOL_TYPE, MT_TOOL_FINGER),
                (EV_ABS, ABS_MT_POSITION_X, 400),
                (EV_ABS, ABS_MT_POSITION_Y, 300),
                (EV_KEY, BTN_TOOL_FINGER, 0),
                (EV_KEY, BTN_TOOL_DOUBLETAP, 1),
                (EV_KEY, BTN_LEFT, 1),
                (EV_SYN, SYN_REPORT, 0),
            ]
        );
        assert_eq!(
            encoder.frame(&[at(5, 400, 300)], false),
            [
                (EV_ABS, ABS_MT_SLOT, 0),
                (EV_ABS, ABS_MT_TRACKING_ID, -1),
                (EV_ABS, ABS_X, 400),
                (EV_ABS, ABS_Y, 300),
                (EV_KEY, BTN_TOOL_DOUBLETAP, 0),
                (EV_KEY, BTN_TOOL_FINGER, 1),
                (EV_KEY, BTN_LEFT, 0),
                (EV_SYN, SYN_REPORT, 0),
            ]
        );
        assert_eq!(
            encoder.frame(&[], false),
            [(EV_ABS, ABS_MT_SLOT, 1), (EV_ABS, ABS_MT_TRACKING_ID, -1), (EV_KEY, BTN_TOUCH, 0), (EV_KEY, BTN_TOOL_FINGER, 0), (EV_SYN, SYN_REPORT, 0)]
        );
    }

    #[test]
    fn palms_are_marked_and_do_not_count_as_fingers() {
        let mut encoder = Encoder::new(5);
        let events = encoder.frame(&[Contact { id: 1, x: 10, y: 10, palm: true }], false);
        assert!(events.contains(&(EV_ABS, ABS_MT_TOOL_TYPE, MT_TOOL_PALM)));
        assert!(!events.iter().any(|(kind, code, _)| *kind == EV_KEY || (*kind == EV_ABS && *code == ABS_X)));
        let events = encoder.frame(&[Contact { id: 1, x: 10, y: 10, palm: false }], false);
        assert!(events.contains(&(EV_ABS, ABS_MT_TOOL_TYPE, MT_TOOL_FINGER)) && events.contains(&(EV_KEY, BTN_TOOL_FINGER, 1)));
    }

    #[test]
    fn a_replayed_tap_comes_and_goes_beside_the_fingers_already_shown() {
        let mut encoder = Encoder::new(5);
        encoder.frame(&[at(1, 50, 50)], false);
        let events = encoder.tap(&[at(1, 50, 50)], 2, 300, 100, 40);
        let ids: Vec<i32> = events.iter().filter(|(_, code, _)| *code == ABS_MT_TRACKING_ID).map(|(_, _, value)| *value).collect();
        assert_eq!(ids, [2, 3, -1, -1]);
        assert!(events.contains(&(EV_ABS, ABS_MT_POSITION_X, 340)));
        assert!(events.contains(&(EV_KEY, BTN_TOOL_TRIPLETAP, 1)) && events.contains(&(EV_KEY, BTN_TOOL_FINGER, 1)));
        assert_eq!(events.iter().filter(|event| **event == (EV_SYN, SYN_REPORT, 0)).count(), 2);
        assert!(encoder.frame(&[at(1, 50, 50)], false).is_empty());
    }

    #[test]
    fn more_touches_than_slots_are_left_out_and_millimetres_map_onto_the_axis() {
        let mut encoder = Encoder::new(2);
        let events = encoder.frame(&[at(1, 1, 1), at(2, 2, 2), at(3, 3, 3)], false);
        assert_eq!(events.iter().filter(|(_, code, _)| *code == ABS_MT_TRACKING_ID).count(), 2);
        assert!(events.contains(&(EV_KEY, BTN_TOOL_DOUBLETAP, 1)));
        let axis = Axis { logical_min: 0, logical_max: 3000, micrometres: 120_000 };
        assert_eq!((logical(&axis, 0.0), logical(&axis, 60.0), logical(&axis, 500.0), logical(&axis, -3.0)), (0, 1500, 3000, 0));
    }
}
