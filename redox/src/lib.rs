use std::io::Read;
use std::path::Path;
use std::time::Instant;

use drv_i18n::{Catalog, Fail, Msg};
use elantp_core::acpi::{self, HID_DSM_DESCRIPTOR_ADDRESS, HID_DSM_REVISION, HID_DSM_UUID, Resource};
use elantp_core::amdgpio::Trigger;
use elantp_core::aml::{self, Value};
use elantp_core::designware::{I2cError, Master};
use elantp_core::hw::{Clock, Registers};
use elantp_core::pad::Bus;
use gestus::{Config, Gestures, Key, Notice, Template};

pub const LOCALES: [(&str, &str); 2] = [("ru", include_str!("../../locales/ru.ftl")), ("en", include_str!("../../locales/en.ftl"))];

pub type BoxError = Box<dyn std::error::Error>;

pub const PAD: &str = "_SB.I2CA.TPDD";
pub const CONTROLLER: &str = "_SB.I2CA";
pub const GPIO: &str = "_SB.GPIO";
const PAYLOAD: usize = 64 * 1024;
pub const SETTINGS: &str = "/etc/elantp/gestures.toml";
pub const SYMBOLS: &str = "/etc/elantp/symbols.toml";
const SETTINGS_LIMIT: u64 = 1024 * 1024;
pub const ABSOLUTE: f32 = 65535.0;

pub fn catalog() -> Catalog {
    Catalog::new(&LOCALES)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resources {
    pub address: u16,
    pub speed: u32,
    pub pin: u16,
    pub trigger: Trigger,
    pub register: u16,
    pub controller: (u32, u32),
    pub gpio: (u32, u32),
    pub irq: u32,
}

pub const VERIFIED: Resources = Resources {
    address: 0x15,
    speed: 400_000,
    pin: 9,
    trigger: Trigger { level: true, active_low: true },
    register: 1,
    controller: (0xfedc_2000, 0x1000),
    gpio: (0xfed8_1500, 0x400),
    irq: 7,
};

pub fn evaluate(symbol: &str, arguments: &str) -> Result<Value, BoxError> {
    let failed = |detail: String| Fail::new("redox-acpi-failed").arg("symbol", symbol).arg("detail", detail);
    let path = format!("/scheme/acpi/symbols/{symbol}");
    let handle = libredox::Fd::open(&path, libredox::flag::O_RDWR | libredox::flag::O_CLOEXEC, 0).map_err(|e| failed(e.to_string()))?;
    let mut payload = vec![b' '; PAYLOAD];
    payload[..arguments.len()].copy_from_slice(arguments.as_bytes());
    let length = handle.call_rw(&mut payload, syscall::CallFlags::empty(), &[]).map_err(|e| failed(e.to_string()))?;
    let text = std::str::from_utf8(payload.get(..length).unwrap_or_default()).map_err(|e| failed(e.to_string()))?;
    Ok(aml::value(text).ok_or_else(|| failed(drv_i18n::printable(text.get(..64).unwrap_or(text))))?)
}

fn template(symbol: &str) -> Result<Vec<Resource>, BoxError> {
    match evaluate(symbol, &aml::no_arguments())? {
        Value::Buffer(bytes) => Ok(acpi::resources(&bytes).map_err(|e| Fail::new("redox-acpi-failed").arg("symbol", symbol).arg("detail", format!("{e:?}")))?),
        Value::Integer(_) => Err(Fail::new("redox-acpi-failed").arg("symbol", symbol).arg("detail", "integer").into()),
    }
}

fn memory(resources: &[Resource], symbol: &str) -> Result<(u32, u32), BoxError> {
    resources
        .iter()
        .find_map(|resource| match resource {
            Resource::Memory { base, length, .. } => Some((*base, *length)),
            _ => None,
        })
        .ok_or_else(|| Fail::new("redox-acpi-missing").arg("symbol", symbol).arg("what", "memory").into())
}

pub fn resources() -> Result<Resources, BoxError> {
    let pad_symbol = format!("{PAD}._CRS");
    let pad = template(&pad_symbol)?;
    let missing = |what: &str| Fail::new("redox-acpi-missing").arg("symbol", &pad_symbol).arg("what", what);
    let (address, speed) = pad
        .iter()
        .find_map(|resource| match resource {
            Resource::I2c { address, speed, .. } => Some((*address, *speed)),
            _ => None,
        })
        .ok_or_else(|| missing("i2c"))?;
    let (pin, trigger) = pad
        .iter()
        .find_map(|resource| match resource {
            Resource::GpioInterrupt { pins, edge, active_low, .. } => pins.first().map(|pin| (*pin, Trigger { level: !edge, active_low: *active_low })),
            _ => None,
        })
        .ok_or_else(|| missing("gpio"))?;
    let dsm = format!("{PAD}._DSM");
    let register = match evaluate(&dsm, &aml::dsm_arguments(&HID_DSM_UUID, HID_DSM_REVISION, HID_DSM_DESCRIPTOR_ADDRESS))? {
        Value::Integer(register) => u16::try_from(register).map_err(|_| Fail::new("redox-acpi-missing").arg("symbol", &dsm).arg("what", "register"))?,
        Value::Buffer(_) => return Err(Fail::new("redox-acpi-missing").arg("symbol", &dsm).arg("what", "register").into()),
    };
    let controller_symbol = format!("{CONTROLLER}._CRS");
    let controller = memory(&template(&controller_symbol)?, &controller_symbol)?;
    let gpio_symbol = format!("{GPIO}._CRS");
    let gpio_resources = template(&gpio_symbol)?;
    let gpio = memory(&gpio_resources, &gpio_symbol)?;
    let irq = gpio_resources
        .iter()
        .find_map(|resource| match resource {
            Resource::Interrupt { numbers, .. } => numbers.first().copied(),
            _ => None,
        })
        .ok_or_else(|| Fail::new("redox-acpi-missing").arg("symbol", &gpio_symbol).arg("what", "interrupt"))?;
    Ok(Resources { address, speed, pin, trigger, register, controller, gpio, irq })
}

pub struct Mmio {
    base: *mut u32,
    words: usize,
}

impl Mmio {
    pub fn map(physical: u32, length: u32) -> Result<Self, BoxError> {
        let protection = common::Prot { read: true, write: true };
        let pointer = unsafe { common::physmap(physical as usize, length as usize, protection, common::MemoryType::Uncacheable) }
            .map_err(|e| Fail::new("redox-map-failed").arg("address", format!("{physical:#x}")).arg("detail", e))?;
        Ok(Mmio { base: pointer.cast(), words: length as usize / 4 })
    }
}

impl Registers for Mmio {
    fn read(&mut self, offset: usize) -> u32 {
        let index = offset / 4;
        if !offset.is_multiple_of(4) || index >= self.words {
            return 0;
        }
        unsafe { std::ptr::read_volatile(self.base.add(index)) }
    }

    fn write(&mut self, offset: usize, value: u32) {
        let index = offset / 4;
        if !offset.is_multiple_of(4) || index >= self.words {
            return;
        }
        unsafe { std::ptr::write_volatile(self.base.add(index), value) }
    }
}

pub struct Monotonic(pub Instant);

impl Clock for Monotonic {
    fn micros(&mut self) -> u64 {
        u64::try_from(self.0.elapsed().as_micros()).unwrap_or(u64::MAX)
    }
}

pub struct PadBus {
    pub master: Master<Mmio, Monotonic>,
    pub address: u16,
}

impl Bus for PadBus {
    type Error = I2cError;

    fn transfer(&mut self, write: &[u8], read: &mut [u8]) -> Result<(), I2cError> {
        self.master.transfer(self.address, write, read)
    }
}

fn read_limited(path: &Path) -> Result<String, Fail> {
    let unreadable = |detail: String| Fail::new("redox-settings-unreadable").arg("file", path.display()).arg("detail", detail);
    let file = std::fs::File::open(path).map_err(|e| unreadable(e.to_string()))?;
    let mut text = String::new();
    file.take(SETTINGS_LIMIT + 1).read_to_string(&mut text).map_err(|e| unreadable(e.to_string()))?;
    if text.len() as u64 > SETTINGS_LIMIT {
        return Err(unreadable(format!("> {SETTINGS_LIMIT}")));
    }
    Ok(text)
}

fn invalid(path: &Path, problem: gestus::config::Problem) -> Fail {
    Fail::new("redox-settings-invalid").arg("file", path.display()).num("line", problem.line).num("column", problem.column).arg("detail", problem.message)
}

pub fn settings(path: &Path, symbols: &Path) -> Result<Option<(Config, Vec<Template>)>, Fail> {
    if !path.exists() {
        return Ok(None);
    }
    let config = gestus::config::parse(&read_limited(path)?).map_err(|problem| invalid(path, problem))?;
    let learned = if symbols.exists() { gestus::config::templates(&read_limited(symbols)?).map_err(|problem| invalid(symbols, problem))? } else { Vec::new() };
    Ok(Some((config, learned)))
}

pub fn notice(gestures: &Gestures, notice: Notice) -> Msg {
    match notice {
        Notice::Touchpad(true) => Msg::new("gesture-touchpad-on"),
        Notice::Touchpad(false) => Msg::new("gesture-touchpad-off"),
        Notice::Numpad(true) => Msg::new("gesture-numpad-on"),
        Notice::Numpad(false) => Msg::new("gesture-numpad-off"),
        Notice::Tablet(true) => Msg::new("gesture-tablet-on"),
        Notice::Tablet(false) => Msg::new("gesture-tablet-off"),
        Notice::Drawing => Msg::new("gesture-drawing"),
        Notice::Symbol(index) => Msg::new("gesture-symbol").arg("name", gestures.symbol(index).unwrap_or_default()),
        Notice::Unrecognized => Msg::new("gesture-unrecognized"),
        Notice::Cancelled => Msg::new("gesture-cancelled"),
    }
}

pub fn scancode(key: Key) -> Option<u8> {
    use orbclient::event::*;
    Some(match key {
        Key::A => K_A,
        Key::B => K_B,
        Key::C => K_C,
        Key::D => K_D,
        Key::E => K_E,
        Key::F => K_F,
        Key::G => K_G,
        Key::H => K_H,
        Key::I => K_I,
        Key::J => K_J,
        Key::K => K_K,
        Key::L => K_L,
        Key::M => K_M,
        Key::N => K_N,
        Key::O => K_O,
        Key::P => K_P,
        Key::Q => K_Q,
        Key::R => K_R,
        Key::S => K_S,
        Key::T => K_T,
        Key::U => K_U,
        Key::V => K_V,
        Key::W => K_W,
        Key::X => K_X,
        Key::Y => K_Y,
        Key::Z => K_Z,
        Key::Digit0 => K_0,
        Key::Digit1 => K_1,
        Key::Digit2 => K_2,
        Key::Digit3 => K_3,
        Key::Digit4 => K_4,
        Key::Digit5 => K_5,
        Key::Digit6 => K_6,
        Key::Digit7 => K_7,
        Key::Digit8 => K_8,
        Key::Digit9 => K_9,
        Key::F1 => K_F1,
        Key::F2 => K_F2,
        Key::F3 => K_F3,
        Key::F4 => K_F4,
        Key::F5 => K_F5,
        Key::F6 => K_F6,
        Key::F7 => K_F7,
        Key::F8 => K_F8,
        Key::F9 => K_F9,
        Key::F10 => K_F10,
        Key::F11 => K_F11,
        Key::F12 => K_F12,
        Key::Escape => K_ESC,
        Key::Tab => K_TAB,
        Key::Enter => K_ENTER,
        Key::Space => K_SPACE,
        Key::Backspace => K_BKSP,
        Key::Delete => K_DEL,
        Key::Insert => K_INS,
        Key::Home => K_HOME,
        Key::End => K_END,
        Key::PageUp => K_PGUP,
        Key::PageDown => K_PGDN,
        Key::Left => K_LEFT,
        Key::Right => K_RIGHT,
        Key::Up => K_UP,
        Key::Down => K_DOWN,
        Key::Minus => K_MINUS,
        Key::Equal => K_EQUALS,
        Key::Comma => K_COMMA,
        Key::Dot => K_PERIOD,
        Key::Slash => K_SLASH,
        Key::Semicolon => K_SEMICOLON,
        Key::Apostrophe => K_QUOTE,
        Key::Grave => K_TICK,
        Key::BracketLeft => K_BRACE_OPEN,
        Key::BracketRight => K_BRACE_CLOSE,
        Key::Backslash => K_BACKSLASH,
        Key::Ctrl => K_LEFT_CTRL,
        Key::Shift => K_LEFT_SHIFT,
        Key::Alt => K_ALT,
        Key::Super => K_LEFT_SUPER,
        Key::Menu => K_APP,
        Key::Kp0 => K_NUM_0,
        Key::Kp1 => K_NUM_1,
        Key::Kp2 => K_NUM_2,
        Key::Kp3 => K_NUM_3,
        Key::Kp4 => K_NUM_4,
        Key::Kp5 => K_NUM_5,
        Key::Kp6 => K_NUM_6,
        Key::Kp7 => K_NUM_7,
        Key::Kp8 => K_NUM_8,
        Key::Kp9 => K_NUM_9,
        Key::KpDot => K_NUM_PERIOD,
        Key::KpPlus => K_NUM_PLUS,
        Key::KpMinus => K_NUM_MINUS,
        Key::KpAsterisk => K_NUM_ASTERISK,
        Key::KpSlash => K_NUM_SLASH,
        Key::KpEnter => K_NUM_ENTER,
        Key::VolumeUp => K_VOLUME_UP,
        Key::VolumeDown => K_VOLUME_DOWN,
        Key::Mute => K_VOLUME_TOGGLE,
        Key::PlayPause => K_MEDIA_PLAY_PAUSE,
        Key::StopMedia => K_MEDIA_STOP,
        Key::NextTrack => K_MEDIA_FAST_FORWARD,
        Key::PreviousTrack => K_MEDIA_REWIND,
        Key::Print => K_PRTSC,
        Key::MicMute | Key::BrightnessUp | Key::BrightnessDown | Key::Calculator | Key::Browser | Key::Mail | Key::Files | Key::Search | Key::LockScreen => {
            return None;
        }
    })
}
