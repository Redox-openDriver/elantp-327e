use std::time::Instant;

use drv_i18n::{Catalog, Fail};
use elantp_core::acpi::{self, HID_DSM_DESCRIPTOR_ADDRESS, HID_DSM_REVISION, HID_DSM_UUID, Resource};
use elantp_core::amdgpio::Trigger;
use elantp_core::aml::{self, Value};
use elantp_core::designware::{I2cError, Master};
use elantp_core::hw::{Clock, Registers};
use elantp_core::pad::Bus;

pub const LOCALES: [(&str, &str); 2] = [("ru", include_str!("../../locales/ru.ftl")), ("en", include_str!("../../locales/en.ftl"))];

pub type BoxError = Box<dyn std::error::Error>;

pub const PAD: &str = "_SB.I2CA.TPDD";
pub const CONTROLLER: &str = "_SB.I2CA";
pub const GPIO: &str = "_SB.GPIO";
const PAYLOAD: usize = 64 * 1024;

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
