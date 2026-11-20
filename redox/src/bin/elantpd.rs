use std::time::{Duration, Instant};

use drv_i18n::{Catalog, Fail, Msg, as_msg};
use elantp_core::amdgpio;
use elantp_core::designware::Master;
use elantp_core::pad::{Pad, RESET_PATIENCE};
use elantp_core::touchpad::Frame;
use elantp_redox::{BoxError, Mmio, Monotonic, PadBus, Resources, VERIFIED};
use event::{EventFlags, RawEventQueue};
use gestus::{Gestures, Input, Output, Shaper, Surface, Touch};
use orbclient::{ButtonEvent, MouseRelativeEvent, ScrollEvent};
use syscall::data::TimeSpec;

const CLOCK_HZ: u32 = 150_000_000;
const BUDGET: u64 = 50_000;
const DRAIN: usize = 16;
const SCROLL_PER_NOTCH: f32 = 30.0;
const ON_IRQ: usize = 1;
const ON_TIMER: usize = 2;
const NANOS_IN_SECOND: u128 = 1_000_000_000;

fn say(catalog: &Catalog, message: &Msg) {
    eprintln!("{}", catalog.render(message));
}

fn arm(timer: &libredox::Fd, lasting: Duration) -> Result<(), BoxError> {
    let mut at = TimeSpec::default();
    timer.read(&mut at)?;
    let nanos = u128::try_from(at.tv_nsec).unwrap_or(0) + lasting.as_nanos();
    at.tv_sec += i64::try_from(nanos / NANOS_IN_SECOND).unwrap_or(i64::MAX);
    at.tv_nsec = i32::try_from(nanos % NANOS_IN_SECOND).unwrap_or(0);
    timer.write(&at)?;
    Ok(())
}

struct Desk {
    producer: inputd::ProducerHandle,
    gestures: Gestures,
    shaper: Shaper,
    started: Instant,
}

impl Desk {
    fn deliver(&mut self, pad: &Pad<PadBus>, frame: &Frame) -> Result<(), BoxError> {
        let touches: Vec<Touch> = frame
            .contacts
            .iter()
            .filter(|contact| contact.tip)
            .map(|contact| {
                let (x, y) = pad.layout.millimetres(contact);
                Touch { id: contact.id, x, y, palm: !contact.confident }
            })
            .collect();
        let time = u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX);
        for event in self.gestures.feed(&Input { time, button: frame.button, touches }) {
            let output = match self.shaper.shape(event, SCROLL_PER_NOTCH) {
                Some(Output::Move { dx, dy }) => MouseRelativeEvent { dx, dy }.to_event(),
                Some(Output::Buttons { left, middle, right }) => ButtonEvent { left, middle, right }.to_event(),
                Some(Output::Wheel { x, y }) => ScrollEvent { x, y }.to_event(),
                None => continue,
            };
            self.producer.write_event(output)?;
        }
        Ok(())
    }
}

fn find_resources(catalog: &Catalog) -> Resources {
    match elantp_redox::resources() {
        Ok(found) => found,
        Err(e) => {
            say(catalog, &as_msg(e.as_ref(), "redox-error"));
            say(catalog, &Msg::new("redox-acpi-fallback"));
            VERIFIED
        }
    }
}

fn run(catalog: &Catalog) -> Result<(), BoxError> {
    let resources = find_resources(catalog);
    let started = Instant::now();
    let controller = Mmio::map(resources.controller.0, resources.controller.1)?;
    let master = Master::new(controller, Monotonic(started), CLOCK_HZ, resources.speed, BUDGET)
        .map_err(|e| Fail::new("redox-controller-failed").arg("detail", format!("{e:?}")))?;
    let bus = PadBus { master, address: resources.address };
    let mut pad = Pad::start(bus, resources.register, 0).map_err(|e| Fail::new("redox-pad-failed").arg("detail", format!("{e:?}")))?;
    let mut gpio = Mmio::map(resources.gpio.0, resources.gpio.1)?;
    amdgpio::arm(&mut gpio, resources.pin, resources.trigger).ok_or_else(|| Fail::new("redox-pin-failed").num("pin", resources.pin))?;
    let irq = libredox::Fd::open(&format!("/scheme/irq/{}", resources.irq), libredox::flag::O_RDWR | libredox::flag::O_CLOEXEC, 0)
        .map_err(|e| Fail::new("redox-irq-failed").num("irq", resources.irq).arg("detail", e))?;
    let clock = format!("/scheme/time/{}", libredox::flag::CLOCK_MONOTONIC);
    let timer = libredox::Fd::open(&clock, libredox::flag::O_NONBLOCK | libredox::flag::O_CLOEXEC, 0)?;
    let events = RawEventQueue::new()?;
    events.subscribe(irq.raw(), ON_IRQ, EventFlags::READ)?;
    events.subscribe(timer.raw(), ON_TIMER, EventFlags::READ)?;
    arm(&timer, Duration::from_micros(RESET_PATIENCE))?;
    let (width, height) = pad.layout.size();
    let mut desk = Desk {
        producer: inputd::ProducerHandle::new().map_err(|e| Fail::new("redox-input-failed").arg("detail", e))?,
        gestures: Gestures::new(gestus::DEFAULT, Surface { width, height }),
        shaper: Shaper::default(),
        started,
    };
    say(catalog, &Msg::new("redox-started").arg("address", format!("{:#04x}", resources.address)).num("pin", resources.pin).num("irq", resources.irq));
    for event in events {
        let event = event?;
        match event.user_data {
            ON_IRQ => {
                let mut counter = [0u8; 8];
                irq.read(&mut counter)?;
                for _ in 0..DRAIN {
                    if !amdgpio::asserted(&mut gpio, resources.pin, resources.trigger) {
                        break;
                    }
                    match pad.interrupt() {
                        Ok(Some(frame)) => desk.deliver(&pad, &frame)?,
                        Ok(None) => {}
                        Err(e) => {
                            say(catalog, &Msg::new("redox-read-failed").arg("detail", format!("{e:?}")));
                            break;
                        }
                    }
                }
                amdgpio::service(&mut gpio, resources.pin);
                irq.write(&counter)?;
            }
            ON_TIMER => {
                let mut now = TimeSpec::default();
                timer.read(&mut now)?;
                let elapsed = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
                pad.tick(elapsed).map_err(|e| Fail::new("redox-pad-failed").arg("detail", format!("{e:?}")))?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn main() {
    let catalog = elantp_redox::catalog();
    if let Err(e) = run(&catalog) {
        say(&catalog, &as_msg(e.as_ref(), "redox-error"));
        std::process::exit(1);
    }
}
