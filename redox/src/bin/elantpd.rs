use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

use drv_i18n::{Catalog, Fail, Msg, as_msg};
use elantp_core::amdgpio;
use elantp_core::designware::Master;
use elantp_core::pad::{Pad, RESET_PATIENCE, State};
use elantp_core::touchpad::Frame;
use elantp_redox::{BoxError, Mmio, Monotonic, PadBus, Resources, VERIFIED};
use event::{EventFlags, RawEventQueue};
use gestus::{Config, Event, Gestures, Input, Key, Output, Shaper, Surface, Template, Touch};
use orbclient::{ButtonEvent, KeyEvent, MouseEvent, MouseRelativeEvent, ScrollEvent};
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

fn arm_in(timer: &libredox::Fd, lasting: Duration) -> Result<(), BoxError> {
    let mut at = TimeSpec::default();
    timer.read(&mut at)?;
    let nanos = u128::try_from(at.tv_nsec).unwrap_or(0) + lasting.as_nanos();
    at.tv_sec += i64::try_from(nanos / NANOS_IN_SECOND).unwrap_or(i64::MAX);
    at.tv_nsec = i32::try_from(nanos % NANOS_IN_SECOND).unwrap_or(0);
    timer.write(&at)?;
    Ok(())
}

struct Desk<'a> {
    catalog: &'a Catalog,
    producer: inputd::ProducerHandle,
    gestures: Gestures,
    shaper: Shaper,
    started: Instant,
    pen_down: bool,
    unsupported: BTreeSet<Key>,
}

impl Desk<'_> {
    fn now(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX)
    }

    fn send(&mut self, events: Vec<Event>) -> Result<(), BoxError> {
        for event in events {
            let output = match event {
                Event::Pen { x, y, down } => {
                    let place = MouseEvent { x: (x * elantp_redox::ABSOLUTE) as i32, y: (y * elantp_redox::ABSOLUTE) as i32 };
                    self.producer.write_event(place.to_event())?;
                    if down != self.pen_down {
                        self.pen_down = down;
                        self.producer.write_event(ButtonEvent { left: down, middle: false, right: false }.to_event())?;
                    }
                    continue;
                }
                Event::PenAway => {
                    if std::mem::take(&mut self.pen_down) {
                        self.producer.write_event(ButtonEvent { left: false, middle: false, right: false }.to_event())?;
                    }
                    continue;
                }
                Event::Notice(notice) => {
                    say(self.catalog, &elantp_redox::notice(&self.gestures, notice));
                    continue;
                }
                other => other,
            };
            let event = match self.shaper.shape(output, SCROLL_PER_NOTCH) {
                Some(Output::Move { dx, dy }) => MouseRelativeEvent { dx, dy }.to_event(),
                Some(Output::Buttons { left, middle, right }) => ButtonEvent { left, middle, right }.to_event(),
                Some(Output::Wheel { x, y }) => ScrollEvent { x, y }.to_event(),
                Some(Output::Key { key, pressed }) => match elantp_redox::scancode(key) {
                    Some(scancode) => KeyEvent { character: '\0', scancode, pressed }.to_event(),
                    None => {
                        if self.unsupported.insert(key) {
                            say(self.catalog, &Msg::new("redox-key-unsupported").arg("key", key.name()));
                        }
                        continue;
                    }
                },
                None => continue,
            };
            self.producer.write_event(event)?;
        }
        Ok(())
    }

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
        let time = self.now();
        let events = self.gestures.feed(&Input { time, button: frame.button, touches });
        self.send(events)
    }

    fn tick(&mut self) -> Result<(), BoxError> {
        let now = self.now();
        if self.gestures.deadline().is_some_and(|at| at <= now) {
            let events = self.gestures.tick(now);
            self.send(events)?;
        }
        Ok(())
    }
}

fn load_settings(catalog: &Catalog) -> (Config, Vec<Template>) {
    match elantp_redox::settings(Path::new(elantp_redox::SETTINGS), Path::new(elantp_redox::SYMBOLS)) {
        Ok(Some(found)) => {
            say(catalog, &Msg::new("redox-settings-used").arg("file", elantp_redox::SETTINGS));
            found
        }
        Ok(None) => (Config::default(), Vec::new()),
        Err(e) => {
            say(catalog, &as_msg(&e, "redox-error"));
            say(catalog, &Msg::new("redox-settings-default"));
            (Config::default(), Vec::new())
        }
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
    arm_in(&timer, Duration::from_micros(RESET_PATIENCE))?;
    let mut armed: Option<u64> = None;
    let (width, height) = pad.layout.size();
    let (config, learned) = load_settings(catalog);
    let mut desk = Desk {
        catalog,
        producer: inputd::ProducerHandle::new().map_err(|e| Fail::new("redox-input-failed").arg("detail", e))?,
        gestures: Gestures::with_symbols(config, Surface { width, height }, &learned),
        shaper: Shaper::default(),
        started,
        pen_down: false,
        unsupported: BTreeSet::new(),
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
                armed = None;
                pad.tick(desk.now()).map_err(|e| Fail::new("redox-pad-failed").arg("detail", format!("{e:?}")))?;
                desk.tick()?;
            }
            _ => {}
        }
        let reset = match pad.state {
            State::Resetting { since } => Some(since.saturating_add(RESET_PATIENCE)),
            State::Running => None,
        };
        if let Some(wake) = [reset, desk.gestures.deadline()].into_iter().flatten().min()
            && armed != Some(wake)
        {
            arm_in(&timer, Duration::from_micros(wake.saturating_sub(desk.now())))?;
            armed = Some(wake);
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
