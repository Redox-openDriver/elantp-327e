use std::io;

use elantp_core::touchpad::{Axis, Frame, Layout};
use gestus::{Button, Event, Surface, Touch};

use crate::{hotkeys, keys, uinput};

pub const SCROLL_PER_NOTCH: f32 = 30.0;

pub fn millimetres(axis: &Axis, value: i64) -> f32 {
    ((value - axis.logical_min) as f64 * axis.micrometres as f64 / 1000.0 / (axis.logical_max - axis.logical_min) as f64) as f32
}

pub fn touches(layout: &Layout, frame: &Frame) -> Vec<Touch> {
    frame
        .contacts
        .iter()
        .filter(|contact| contact.tip)
        .map(|contact| Touch { id: contact.id, x: millimetres(&layout.x, contact.x), y: millimetres(&layout.y, contact.y), palm: !contact.confident })
        .collect()
}

pub fn surface(layout: &Layout) -> Surface {
    Surface { width: layout.x.micrometres as f32 / 1000.0, height: layout.y.micrometres as f32 / 1000.0 }
}

pub fn button_code(button: Button) -> u16 {
    match button {
        Button::Left => uinput::BTN_LEFT,
        Button::Right => uinput::BTN_RIGHT,
        Button::Middle => uinput::BTN_MIDDLE,
    }
}

pub struct Devices {
    pointer: uinput::Pointer,
    keyboard: uinput::Keyboard,
    pen: Option<uinput::Pen>,
    surface: Surface,
}

impl Devices {
    pub fn create(surface: Surface, tablet: bool) -> io::Result<Self> {
        let mut devices = Devices {
            pointer: uinput::Pointer::create("elantp gestus")?,
            keyboard: uinput::Keyboard::create(
                "elantp gestus keys",
                gestus::Key::ALL.iter().map(|key| keys::code(*key)).chain([hotkeys::ANNOUNCE_ON, hotkeys::ANNOUNCE_OFF]),
            )?,
            pen: None,
            surface,
        };
        devices.tablet(tablet)?;
        Ok(devices)
    }

    pub fn tablet(&mut self, on: bool) -> io::Result<()> {
        if !on {
            self.pen = None;
        } else if self.pen.is_none() {
            self.pen = Some(uinput::Pen::create("elantp gestus pen", self.surface.width, self.surface.height)?);
        }
        Ok(())
    }

    pub fn send(&mut self, event: Event) -> io::Result<Option<Event>> {
        match event {
            Event::Motion { dx, dy } => self.pointer.motion(dx, dy)?,
            Event::Press(button) => self.pointer.button(button_code(button), true)?,
            Event::Release(button) => self.pointer.button(button_code(button), false)?,
            Event::Scroll { dx, dy } => self.pointer.scroll(dx, dy, SCROLL_PER_NOTCH)?,
            Event::Key { key, pressed } => self.keyboard.key(keys::code(key), pressed)?,
            Event::Pen { x, y, down } => {
                if let Some(pen) = self.pen.as_mut() {
                    pen.place(x, y, down)?;
                }
            }
            Event::PenAway => {
                if let Some(pen) = self.pen.as_mut() {
                    pen.away()?;
                }
            }
            other => return Ok(Some(other)),
        }
        Ok(None)
    }

    pub fn announce(&mut self, on: bool) -> io::Result<()> {
        let code = hotkeys::announcement(on);
        self.keyboard.key(code, true)?;
        self.keyboard.key(code, false)
    }

    pub fn let_go(&mut self) -> io::Result<()> {
        for button in [Button::Left, Button::Right, Button::Middle] {
            self.pointer.button(button_code(button), false)?;
        }
        for key in keys::HELD {
            self.keyboard.key(keys::code(key), false)?;
        }
        if let Some(pen) = self.pen.as_mut() {
            pen.away()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_units_turn_into_millimetres_across_the_axis() {
        let axis = Axis { logical_min: 0, logical_max: 3000, micrometres: 117_000 };
        assert!((millimetres(&axis, 1500) - 58.5).abs() < 1e-4);
        assert_eq!((millimetres(&axis, 0), millimetres(&axis, 3000)), (0.0, 117.0));
        let shifted = Axis { logical_min: 100, logical_max: 1100, micrometres: 50_000 };
        assert!((millimetres(&shifted, 600) - 25.0).abs() < 1e-4);
    }
}
