#![no_std]

extern crate alloc;

#[cfg(feature = "config")]
pub mod config;
mod engine;
mod filter;
mod geometry;
mod inertia;
mod keys;
mod settings;
mod shaper;
mod symbols;
mod taps;
mod zones;

use alloc::vec::Vec;

pub use engine::Gestures;
pub use keys::{Action, COMBO, Combo, Key, Slider, Toggle};
pub use settings::*;
pub use shaper::{Output, Shaper};
pub use symbols::{POINTS, Template, normalize};
pub use zones::{Corner, Edge};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Touch {
    pub id: u8,
    pub x: f32,
    pub y: f32,
    pub palm: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    pub time: u64,
    pub button: bool,
    pub touches: Vec<Touch>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Surface {
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Begin,
    Update,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    Touchpad(bool),
    Numpad(bool),
    Tablet(bool),
    Drawing,
    Symbol(u16),
    Unrecognized,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    Motion { dx: f32, dy: f32 },
    Press(Button),
    Release(Button),
    Scroll { dx: f32, dy: f32 },
    Swipe { fingers: u8, phase: Phase, dx: f32, dy: f32 },
    Key { key: Key, pressed: bool },
    Pen { x: f32, y: f32, down: bool },
    PenAway,
    Touch { fingers: u8, x: f32, y: f32 },
    Notice(Notice),
}
