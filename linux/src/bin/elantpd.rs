use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use drv_i18n::{Catalog, Fail, Msg, as_msg};
use elantp_core::identity;
use elantp_core::report::ReportDescriptor;
use elantp_core::touchpad::{Assembler, Layout};
use elantp_linux::clone::Touchpad;
use elantp_linux::config::{self, Loaded};
use elantp_linux::evdev::{self, Grab};
use elantp_linux::hidraw::{self, Device, Found, MAX_REPORT};
use elantp_linux::hotkeys::Switches;
use elantp_linux::keyboard::{self, Keyboards};
use elantp_linux::notices::{self, Notices};
use elantp_linux::output::{Devices, surface, touches};
use gestus::{Config, Event, Gestures, Input, Notice, Surface, Toggle};

type BoxError = Box<dyn std::error::Error>;
type Stamp = Option<(SystemTime, Option<SystemTime>)>;

const POLL: Duration = Duration::from_millis(100);
const RELOAD: Duration = Duration::from_secs(1);
const RETRY: Duration = Duration::from_secs(2);

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

struct Outputs {
    touchpad: Touchpad,
    devices: Devices,
    button: bool,
}

impl Outputs {
    fn deliver(&mut self, catalog: &Catalog, engine: &Gestures, events: Vec<Event>, listeners: &mut Option<Notices>) -> Result<(), BoxError> {
        for event in events {
            match self.devices.send(event)? {
                Some(Event::Touch { fingers, x, y }) => self.touchpad.replay(fingers, x, y)?,
                Some(Event::Notice(notice)) => {
                    say(catalog, &elantp_linux::notice(engine, notice));
                    if let Some(listeners) = listeners.as_mut() {
                        listeners.send(&notices::line(engine, notice));
                    }
                    if let Notice::Touchpad(on) = notice {
                        self.devices.announce(on)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn show(&mut self, engine: &Gestures) -> Result<(), BoxError> {
        let button = self.button && !engine.hidden();
        Ok(self.touchpad.show(&engine.visible(), button)?)
    }

    fn let_go(&mut self) -> Result<(), BoxError> {
        Ok(self.devices.let_go()?)
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

fn listen(catalog: &Catalog, keyboards: &mut Keyboards, config: &Config) -> bool {
    if !config.uses_typing(true) {
        keyboards.clear();
        return false;
    }
    keyboards.refresh();
    let message = if keyboards.count() == 0 { Msg::new("service-keyboards-none") } else { Msg::new("service-keyboards").num("count", keyboards.count()) };
    say(catalog, &message);
    true
}

fn open_notices(catalog: &Catalog) -> Option<Notices> {
    match Notices::open(Path::new(notices::SOCKET)) {
        Ok(opened) => {
            say(catalog, &Msg::new("service-notices").arg("socket", notices::SOCKET));
            Some(opened)
        }
        Err(e) => {
            say(catalog, &Msg::new("service-notices-failed").arg("socket", notices::SOCKET).arg("detail", e));
            None
        }
    }
}

fn serve(catalog: &Catalog, path: &Path, loaded: &Loaded, mut stamp: Stamp, listeners: &mut Option<Notices>) -> Result<Outcome, BoxError> {
    let Some(pad) = open_pad()? else { return Ok(Outcome::Foreign) };
    let surface: Surface = surface(&pad.layout);
    let failed = |e: std::io::Error| Fail::new("dev-uinput-failed").arg("detail", e);
    let mut outputs = Outputs {
        touchpad: Touchpad::create(&pad.identity, &pad.layout.x, &pad.layout.y, pad.layout.slots.len()).map_err(failed)?,
        devices: Devices::create(surface, loaded.config.tablet.enabled).map_err(failed)?,
        button: false,
    };
    let grab: Grab = evdev::grab(&pad.nodes).map_err(|e| Fail::new("dev-grab-failed").arg("detail", e))?;
    say(catalog, &Msg::new("service-started").arg("device", &pad.identity.name).arg("node", pad.found.node.display()));
    let mut engine = build(loaded, surface);
    let mut keyboards = Keyboards::default();
    let mut listening = listen(catalog, &mut keyboards, &loaded.config);
    let mut switches = Switches::default();
    switches.refresh();
    if switches.count() > 0 {
        say(catalog, &Msg::new("service-switches").num("count", switches.count()));
    }
    let mut assembler = Assembler::default();
    let mut buffer = vec![0u8; MAX_REPORT];
    let started = Instant::now();
    let mut checked = Instant::now();
    let outcome = loop {
        if STOP.load(Ordering::Relaxed) {
            break Outcome::Stopped;
        }
        let wait = engine.deadline().map_or(POLL, |at| Duration::from_millis(at.saturating_sub(micros(started)).div_ceil(1000)).min(POLL));
        let typing = keyboards.fds();
        let mut fds = vec![pad.device.raw()];
        fds.extend(&typing);
        fds.extend(switches.fds());
        let ready = keyboard::wait(&fds, wait)?;
        if ready[0] & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe).into());
        }
        let (typed, switched) = ready[1..].split_at(typing.len());
        if typed.iter().any(|events| *events != 0) && keyboards.typed() {
            engine.typed(micros(started));
        }
        if switched.iter().any(|events| *events != 0) {
            for on in switches.pressed() {
                let events = engine.switch(Toggle::Touchpad, on, micros(started));
                outputs.deliver(catalog, &engine, events, listeners)?;
                outputs.show(&engine)?;
            }
        }
        if ready[0] & libc::POLLIN != 0 {
            while let Some(length) = pad.device.read(&mut buffer)? {
                if length == 0 {
                    break;
                }
                let Some(frame) = pad.layout.decode(&buffer[..length]).ok().and_then(|report| assembler.push(report)) else { continue };
                outputs.button = frame.button;
                let events = engine.feed(&Input { time: micros(started), button: frame.button, touches: touches(&pad.layout, &frame) });
                outputs.deliver(catalog, &engine, events, listeners)?;
                outputs.show(&engine)?;
            }
        }
        let now = micros(started);
        if engine.deadline().is_some_and(|at| at <= now) {
            let events = engine.tick(now);
            outputs.deliver(catalog, &engine, events, listeners)?;
            outputs.show(&engine)?;
        }
        if checked.elapsed() >= RELOAD {
            checked = Instant::now();
            if listening {
                keyboards.refresh();
            }
            switches.refresh();
            let fresh = config::stamp(path);
            if fresh != stamp {
                stamp = fresh;
                match settings(catalog, path) {
                    Ok(loaded) if !loaded.config.native_extras() => break Outcome::Idle,
                    Ok(loaded) => {
                        outputs.let_go()?;
                        outputs.devices.tablet(loaded.config.tablet.enabled)?;
                        let kept = (!engine.locked(), engine.numpad(), engine.tablet());
                        engine = build(&loaded, surface);
                        let now = micros(started);
                        for (toggle, on) in [(Toggle::Touchpad, kept.0), (Toggle::Numpad, kept.1), (Toggle::Tablet, kept.2 && loaded.config.tablet.enabled)] {
                            let _ = engine.switch(toggle, Some(on), now);
                        }
                        listening = listen(catalog, &mut keyboards, &loaded.config);
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
    let mut listeners = open_notices(&catalog);
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
        match serve(&catalog, &path, &loaded, stamp, &mut listeners) {
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
