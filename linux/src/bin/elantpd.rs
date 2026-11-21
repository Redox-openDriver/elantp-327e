use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use drv_i18n::{Catalog, Fail, Msg, as_msg};
use elantp_core::identity;
use elantp_core::report::ReportDescriptor;
use elantp_core::touchpad::{Assembler, Axis, Frame, Layout};
use elantp_linux::clone::Touchpad;
use elantp_linux::config::{self, Loaded};
use elantp_linux::evdev::{self, Grab};
use elantp_linux::hidraw::{self, Device, Found, MAX_REPORT};
use elantp_linux::{keys, uinput};
use gestus::{Button, Event, Gestures, Input, Surface, Touch};

type BoxError = Box<dyn std::error::Error>;
type Stamp = Option<(SystemTime, Option<SystemTime>)>;

const POLL: Duration = Duration::from_millis(100);
const RELOAD: Duration = Duration::from_secs(1);
const RETRY: Duration = Duration::from_secs(2);
const SCROLL_PER_NOTCH: f32 = 30.0;

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

enum Outcome {
    Stopped,
    Idle,
    Foreign,
}

fn say(catalog: &Catalog, message: &Msg) {
    eprintln!("{}", catalog.render(message));
}

fn nap(lasting: Duration) {
    let until = Instant::now() + lasting;
    while !STOP.load(Ordering::Relaxed) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn micros(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

fn settings(catalog: &Catalog, path: &Path) -> Result<Loaded, Fail> {
    if !path.exists() {
        return Ok(Loaded::default());
    }
    let loaded = config::load(path)?;
    say(catalog, &Msg::new("service-settings").arg("file", path.display()));
    Ok(loaded)
}

fn button_code(button: Button) -> u16 {
    match button {
        Button::Left => uinput::BTN_LEFT,
        Button::Right => uinput::BTN_RIGHT,
        Button::Middle => uinput::BTN_MIDDLE,
    }
}

fn millimetres(axis: &Axis, value: i64) -> f32 {
    ((value - axis.logical_min) as f64 * axis.micrometres as f64 / 1000.0 / (axis.logical_max - axis.logical_min) as f64) as f32
}

fn touches(layout: &Layout, frame: &Frame) -> Vec<Touch> {
    frame
        .contacts
        .iter()
        .filter(|contact| contact.tip)
        .map(|contact| Touch { id: contact.id, x: millimetres(&layout.x, contact.x), y: millimetres(&layout.y, contact.y), palm: !contact.confident })
        .collect()
}

struct Outputs {
    touchpad: Touchpad,
    pointer: uinput::Pointer,
    keyboard: uinput::Keyboard,
    pen: Option<uinput::Pen>,
    pen_failed: bool,
    surface: Surface,
    button: bool,
}

impl Outputs {
    fn deliver(&mut self, catalog: &Catalog, engine: &Gestures, events: Vec<Event>) -> Result<(), BoxError> {
        for event in events {
            match event {
                Event::Motion { dx, dy } => self.pointer.motion(dx, dy)?,
                Event::Press(button) => self.pointer.button(button_code(button), true)?,
                Event::Release(button) => self.pointer.button(button_code(button), false)?,
                Event::Scroll { dx, dy } => self.pointer.scroll(dx, dy, SCROLL_PER_NOTCH)?,
                Event::Key { key, pressed } => self.keyboard.key(keys::code(key), pressed)?,
                Event::Touch { fingers, x, y } => self.touchpad.tap(&engine.visible(), fingers, x, y)?,
                Event::Pen { x, y, down } => {
                    if self.pen.is_none() && !self.pen_failed {
                        match uinput::Pen::create("elantp gestus pen", self.surface.width, self.surface.height) {
                            Ok(pen) => self.pen = Some(pen),
                            Err(e) => {
                                self.pen_failed = true;
                                say(catalog, &Msg::new("service-error").arg("detail", e));
                            }
                        }
                    }
                    if let Some(pen) = self.pen.as_mut() {
                        pen.place(x, y, down)?;
                    }
                }
                Event::PenAway => {
                    if let Some(pen) = self.pen.as_mut() {
                        pen.away()?;
                    }
                }
                Event::Notice(notice) => say(catalog, &elantp_linux::notice(engine, notice)),
                Event::Swipe { .. } => {}
            }
        }
        Ok(())
    }

    fn show(&mut self, engine: &Gestures) -> Result<(), BoxError> {
        let button = self.button && !engine.hidden();
        Ok(self.touchpad.show(&engine.visible(), button)?)
    }

    fn let_go(&mut self) -> Result<(), BoxError> {
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

struct Pad {
    found: Found,
    layout: Layout,
    device: Device,
    nodes: Vec<PathBuf>,
    identity: evdev::Identity,
}

fn open_pad() -> Result<Option<Pad>, BoxError> {
    let Some(found) = hidraw::find(identity::VENDOR_ID, identity::PRODUCT_ID)? else {
        return Err(Fail::new("service-no-touchpad").into());
    };
    if identity::verify(found.vendor, found.product, &found.descriptor).is_err() {
        return Ok(None);
    }
    let parsed = ReportDescriptor::parse(&found.descriptor).map_err(|e| Fail::new("dev-bad-descriptor").arg("detail", format!("{e:?}")))?;
    let layout = Layout::from_descriptor(&parsed).map_err(|e| Fail::new("dev-not-touchpad").arg("detail", format!("{e:?}")))?;
    let device = Device::open(&found.node).map_err(|e| Fail::new("dev-open-failed").arg("node", found.node.display()).arg("detail", e))?;
    let nodes = evdev::nodes(&evdev::hid_prefix(found.bus, found.vendor, found.product))?;
    let touchpad = nodes.iter().find(|node| evdev::multitouch(node)).ok_or_else(|| Fail::new("service-no-touchpad"))?;
    let identity = evdev::identity(touchpad).ok_or_else(|| Fail::new("service-no-touchpad"))?;
    Ok(Some(Pad { found, layout, device, nodes, identity }))
}

fn build(loaded: &Loaded, surface: Surface) -> Gestures {
    Gestures::with_symbols(loaded.config.clone(), surface, &loaded.symbols).native()
}

fn serve(catalog: &Catalog, path: &Path, loaded: &Loaded, mut stamp: Stamp) -> Result<Outcome, BoxError> {
    let Some(pad) = open_pad()? else { return Ok(Outcome::Foreign) };
    let surface = Surface { width: pad.layout.x.micrometres as f32 / 1000.0, height: pad.layout.y.micrometres as f32 / 1000.0 };
    let failed = |e: std::io::Error| Fail::new("dev-uinput-failed").arg("detail", e);
    let mut outputs = Outputs {
        touchpad: Touchpad::create(&pad.identity, &pad.layout.x, &pad.layout.y, pad.layout.slots.len()).map_err(failed)?,
        pointer: uinput::Pointer::create("elantp gestus").map_err(failed)?,
        keyboard: uinput::Keyboard::create("elantp gestus keys", gestus::Key::ALL.iter().map(|key| keys::code(*key))).map_err(failed)?,
        pen: None,
        pen_failed: false,
        surface,
        button: false,
    };
    let grab: Grab = evdev::grab(&pad.nodes).map_err(|e| Fail::new("dev-grab-failed").arg("detail", e))?;
    say(catalog, &Msg::new("service-started").arg("device", &pad.identity.name).arg("node", pad.found.node.display()));
    let mut engine = build(loaded, surface);
    let mut assembler = Assembler::default();
    let mut buffer = vec![0u8; MAX_REPORT];
    let started = Instant::now();
    let mut checked = Instant::now();
    let outcome = loop {
        if STOP.load(Ordering::Relaxed) {
            break Outcome::Stopped;
        }
        let wait = engine.deadline().map_or(POLL, |at| Duration::from_millis(at.saturating_sub(micros(started)).div_ceil(1000)).min(POLL));
        if pad.device.wait(wait)? {
            while let Some(length) = pad.device.read(&mut buffer)? {
                if length == 0 {
                    break;
                }
                let Some(frame) = pad.layout.decode(&buffer[..length]).ok().and_then(|report| assembler.push(report)) else { continue };
                outputs.button = frame.button;
                let events = engine.feed(&Input { time: micros(started), button: frame.button, touches: touches(&pad.layout, &frame) });
                outputs.deliver(catalog, &engine, events)?;
                outputs.show(&engine)?;
            }
        }
        let now = micros(started);
        if engine.deadline().is_some_and(|at| at <= now) {
            let events = engine.tick(now);
            outputs.deliver(catalog, &engine, events)?;
            outputs.show(&engine)?;
        }
        if checked.elapsed() >= RELOAD {
            checked = Instant::now();
            let fresh = config::stamp(path);
            if fresh != stamp {
                stamp = fresh;
                match settings(catalog, path) {
                    Ok(loaded) if !loaded.config.native_extras() => break Outcome::Idle,
                    Ok(loaded) => {
                        outputs.let_go()?;
                        engine = build(&loaded, surface);
                        outputs.show(&engine)?;
                        say(catalog, &Msg::new("service-reloaded").arg("file", path.display()));
                    }
                    Err(e) => {
                        say(catalog, &as_msg(&e, "service-error"));
                        say(catalog, &Msg::new("service-kept"));
                    }
                }
            }
        }
    };
    outputs.let_go()?;
    drop(grab);
    drop(outputs);
    Ok(outcome)
}

fn main() -> ExitCode {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        let handler: extern "C" fn(libc::c_int) = stop;
        libc::signal(libc::SIGINT, handler as libc::sighandler_t);
        libc::signal(libc::SIGTERM, handler as libc::sighandler_t);
    }
    let catalog = elantp_linux::catalog();
    let path = PathBuf::from(config::SYSTEM);
    let mut idle = false;
    while !STOP.load(Ordering::Relaxed) {
        let stamp = config::stamp(&path);
        let loaded = match settings(&catalog, &path) {
            Ok(loaded) => loaded,
            Err(e) => {
                say(&catalog, &as_msg(&e, "service-error"));
                Loaded::default()
            }
        };
        if !loaded.config.native_extras() {
            if !idle {
                say(&catalog, &Msg::new("service-idle").arg("file", path.display()));
                idle = true;
            }
            while !STOP.load(Ordering::Relaxed) && config::stamp(&path) == stamp {
                nap(RELOAD);
            }
            continue;
        }
        idle = false;
        match serve(&catalog, &path, &loaded, stamp) {
            Ok(Outcome::Stopped) => break,
            Ok(Outcome::Idle) => {}
            Ok(Outcome::Foreign) => {
                say(&catalog, &Msg::new("service-foreign"));
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                say(&catalog, &as_msg(e.as_ref(), "service-error"));
                say(&catalog, &Msg::new("service-retry").num("seconds", RETRY.as_secs()));
                nap(RETRY);
            }
        }
    }
    say(&catalog, &Msg::new("service-stopped"));
    ExitCode::SUCCESS
}
