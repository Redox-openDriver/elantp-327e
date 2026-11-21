use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, ErrorKind, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use drv_i18n::{Catalog, Fail, Msg, as_msg};
use elantp_core::identity::{self, Unverified};
use elantp_core::report::ReportDescriptor;
use elantp_core::touchpad::{Assembler, Axis, Frame, Layout, PadType};
use elantp_linux::config::{self, Loaded};
use elantp_linux::hidraw::{self, Device, Found, MAX_REPORT};
use elantp_linux::keyboard::{self, Keyboards};
use elantp_linux::session::{self, Point, SCENARIOS, Scenario, Stats};
use elantp_linux::{evdev, keys, recording, uinput};
use gestus::{Button, Event, Gestures, Input, Phase, Surface, Touch};

type BoxError = Box<dyn std::error::Error>;

const POLL: Duration = Duration::from_millis(100);
const RELOAD: Duration = Duration::from_secs(1);
const SCROLL_PER_NOTCH: f32 = 30.0;
const REPLAY_TAIL: u64 = 10_000_000;
const TICK_LIMIT: usize = 1_000_000;
const LEARN_TIMES: usize = 3;
const MAX_LEARN_TIMES: usize = 10;
const SHORTEST_STROKE: f32 = 8.0;

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

fn catch_interrupt() {
    let handler: extern "C" fn(libc::c_int) = stop;
    unsafe {
        libc::signal(libc::SIGINT, handler as libc::sighandler_t);
        libc::signal(libc::SIGTERM, handler as libc::sighandler_t);
    }
}

fn release_interrupt() {
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_DFL);
        libc::signal(libc::SIGTERM, libc::SIG_DFL);
    }
}

fn device_id() -> String {
    format!("{:04x}:{:04x}", identity::VENDOR_ID, identity::PRODUCT_ID)
}

fn found() -> Result<Found, BoxError> {
    hidraw::find(identity::VENDOR_ID, identity::PRODUCT_ID)?.ok_or_else(|| Fail::new("dev-not-found").arg("device", device_id()).into())
}

fn layout(descriptor: &[u8]) -> Result<Layout, BoxError> {
    let parsed = ReportDescriptor::parse(descriptor).map_err(|e| Fail::new("dev-bad-descriptor").arg("detail", format!("{e:?}")))?;
    Ok(Layout::from_descriptor(&parsed).map_err(|e| Fail::new("dev-not-touchpad").arg("detail", format!("{e:?}")))?)
}

fn verification(vendor: u16, product: u16, descriptor: &[u8]) -> Msg {
    match identity::verify(vendor, product, descriptor) {
        Ok(()) => Msg::new("dev-verified").arg("device", device_id()).num("length", descriptor.len()),
        Err(Unverified::Vendor(found)) => Msg::new("dev-unverified").sub("reason", "dev-unverified-vendor").arg("found", format!("{found:04x}")),
        Err(Unverified::Product(found)) => Msg::new("dev-unverified").sub("reason", "dev-unverified-product").arg("found", format!("{found:04x}")),
        Err(Unverified::Descriptor { length, digest }) => {
            Msg::new("dev-unverified").sub("reason", "dev-unverified-descriptor").num("length", length).arg("found", format!("{digest:016x}"))
        }
    }
}

fn open(found: &Found) -> Result<Device, BoxError> {
    Device::open(&found.node).map_err(|e| {
        let fail = if e.kind() == ErrorKind::PermissionDenied { Fail::new("dev-needs-root") } else { Fail::new("dev-open-failed").arg("detail", e) };
        fail.arg("node", found.node.display()).into()
    })
}

fn hand_over_path(path: &Path) -> Result<(), BoxError> {
    let handle = File::open(path)?;
    hand_over(&handle)
}

fn hand_over(file: &File) -> Result<(), BoxError> {
    let id = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<libc::uid_t>().ok());
    if unsafe { libc::geteuid() } != 0 {
        return Ok(());
    }
    if let (Some(uid), Some(gid)) = (id("SUDO_UID"), id("SUDO_GID"))
        && unsafe { libc::fchown(file.as_raw_fd(), uid, gid) } != 0
    {
        return Err(Fail::new("dev-chown-failed").arg("detail", std::io::Error::last_os_error()).into());
    }
    Ok(())
}

fn millimetres(axis: &Axis, value: i64) -> String {
    let span = (axis.logical_max - axis.logical_min) as f64;
    format!("{:.1}", (value - axis.logical_min) as f64 * axis.micrometres as f64 / 1000.0 / span)
}

fn show_frame(catalog: &Catalog, layout: &Layout, micros: u64, frame: &Frame) {
    let contacts: Vec<String> = frame
        .contacts
        .iter()
        .map(|contact| {
            let state = match (contact.tip, contact.confident) {
                (false, _) => "dev-contact-lifted",
                (true, false) => "dev-contact-palm",
                (true, true) => "dev-contact-touching",
            };
            catalog.render(
                &Msg::new("dev-contact")
                    .num("id", contact.id)
                    .arg("x", millimetres(&layout.x, contact.x))
                    .arg("y", millimetres(&layout.y, contact.y))
                    .sub("state", state),
            )
        })
        .collect();
    let button = if frame.button { "dev-button-down" } else { "dev-button-up" };
    println!(
        "{}",
        catalog.render(
            &Msg::new("dev-frame")
                .arg("time", format!("{:.1}", micros as f64 / 1000.0))
                .num("count", frame.contacts.len())
                .sub("button", button)
                .arg("contacts", contacts.join("  "))
        )
    );
}

fn info(catalog: &Catalog) -> Result<(), BoxError> {
    let found = found()?;
    println!("{}", catalog.render(&Msg::new("dev-found").arg("node", found.node.display()).arg("name", &found.name)));
    println!("{}", catalog.render(&verification(found.vendor, found.product, &found.descriptor)));
    let layout = layout(&found.descriptor)?;
    println!(
        "{}",
        catalog.render(
            &Msg::new("dev-layout")
                .arg("report", format!("{:#04x}", layout.report))
                .num("slots", layout.slots.len())
                .sub("button", if layout.button.is_some() { "dev-yes" } else { "dev-no" })
                .arg("width", format!("{:.1}", layout.x.micrometres as f64 / 1000.0))
                .arg("height", format!("{:.1}", layout.y.micrometres as f64 / 1000.0))
                .arg("resolution", format!("{:.1}", layout.x.units_per_millimetre()))
        )
    );
    let Some(maximum) = layout.maximum else { return Ok(()) };
    let device = match Device::open(&found.node) {
        Ok(device) => device,
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            println!("{}", catalog.render(&Msg::new("dev-more-as-root")));
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    let feature = device.feature(maximum.report, 1 + layout.feature_length(maximum.report).unwrap_or(1))?;
    let kind = match layout.pad_type(&feature) {
        Some(PadType::Clickpad) => "dev-pad-clickpad",
        Some(PadType::Pressure) => "dev-pad-pressure",
        Some(PadType::NonClickable) => "dev-pad-buttons",
        Some(PadType::Other(_)) | None => "dev-pad-unknown",
    };
    let contacts = layout.maximum_contacts(&feature).unwrap_or(0);
    println!("{}", catalog.render(&Msg::new("dev-capabilities").num("contacts", contacts).sub("kind", kind)));
    Ok(())
}

fn pump(device: &Device, limit: Option<Duration>, mut each: impl FnMut(u64, &[u8]) -> Result<(), BoxError>) -> Result<u64, BoxError> {
    let start = Instant::now();
    let mut buffer = vec![0u8; MAX_REPORT];
    let mut count = 0u64;
    while !STOP.load(Ordering::Relaxed) && limit.is_none_or(|limit| start.elapsed() < limit) {
        if !device.wait(POLL)? {
            continue;
        }
        while let Some(length) = device.read(&mut buffer)? {
            if length == 0 {
                break;
            }
            each(u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX), &buffer[..length])?;
            count += 1;
        }
    }
    Ok(count)
}

fn record(catalog: &Catalog, path: &Path, seconds: Option<u64>) -> Result<(), BoxError> {
    let found = found()?;
    layout(&found.descriptor)?;
    let device = open(&found)?;
    let file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).map_err(|e| {
        let fail = if e.kind() == ErrorKind::AlreadyExists { Fail::new("dev-exists") } else { Fail::new("dev-write-failed").arg("detail", e) };
        fail.arg("file", path.display())
    })?;
    hand_over(&file)?;
    let mut out = BufWriter::new(file);
    recording::header(&mut out, &found.descriptor)?;
    catch_interrupt();
    println!("{}", catalog.render(&Msg::new("dev-recording").arg("file", path.display())));
    let started = Instant::now();
    let count = pump(&device, seconds.map(Duration::from_secs), |micros, bytes| Ok(recording::report(&mut out, micros, bytes)?))?;
    out.flush()?;
    println!("{}", catalog.render(&Msg::new("dev-recorded").num("count", count).num("seconds", started.elapsed().as_secs())));
    Ok(())
}

fn show(catalog: &Catalog, path: &Path) -> Result<(), BoxError> {
    let file = File::open(path).map_err(|e| Fail::new("dev-read-failed").arg("file", path.display()).arg("detail", e))?;
    let recorded =
        recording::read(&mut BufReader::new(file)).map_err(|e| Fail::new("dev-bad-recording").arg("file", path.display()).arg("detail", format!("{e:?}")))?;
    println!("{}", catalog.render(&verification(identity::VENDOR_ID, identity::PRODUCT_ID, &recorded.descriptor)));
    let layout = layout(&recorded.descriptor)?;
    let mut assembler = Assembler::default();
    let (mut frames, mut skipped) = (0usize, 0usize);
    for (micros, bytes) in &recorded.reports {
        match layout.decode(bytes) {
            Ok(report) => {
                if let Some(frame) = assembler.push(report) {
                    show_frame(catalog, &layout, *micros, &frame);
                    frames += 1;
                }
            }
            Err(_) => skipped += 1,
        }
    }
    println!("{}", catalog.render(&Msg::new("dev-shown").num("frames", frames).num("skipped", skipped)));
    Ok(())
}

fn touches(layout: &Layout, frame: &Frame) -> Vec<Touch> {
    let millimetres =
        |axis: &Axis, value: i64| ((value - axis.logical_min) as f64 * axis.micrometres as f64 / 1000.0 / (axis.logical_max - axis.logical_min) as f64) as f32;
    frame
        .contacts
        .iter()
        .filter(|contact| contact.tip)
        .map(|contact| Touch { id: contact.id, x: millimetres(&layout.x, contact.x), y: millimetres(&layout.y, contact.y), palm: !contact.confident })
        .collect()
}

fn surface(layout: &Layout) -> Surface {
    Surface { width: layout.x.micrometres as f32 / 1000.0, height: layout.y.micrometres as f32 / 1000.0 }
}

fn button_key(button: Button) -> &'static str {
    match button {
        Button::Left => "dev-gesture-left",
        Button::Right => "dev-gesture-right",
        Button::Middle => "dev-gesture-middle",
    }
}

fn phase_key(phase: Phase) -> &'static str {
    match phase {
        Phase::Begin => "dev-gesture-begin",
        Phase::Update => "dev-gesture-update",
        Phase::End => "dev-gesture-end",
    }
}

#[derive(Default)]
struct Tally {
    clicks: [usize; 3],
    travel: f32,
    scroll: (f32, f32),
    swipes: usize,
    keys: usize,
    notices: usize,
    pens: usize,
    handed: usize,
    touched: usize,
    hidden: usize,
    presses: usize,
    passed: usize,
}

struct Replay<'a> {
    catalog: &'a Catalog,
    quiet: bool,
    tally: Tally,
    motion: (f32, f32),
}

impl Replay<'_> {
    fn flush_motion(&mut self) {
        if !self.quiet && self.motion != (0.0, 0.0) {
            let message = Msg::new("dev-gesture-motion").arg("dx", format!("{:.0}", self.motion.0)).arg("dy", format!("{:.0}", self.motion.1));
            println!("{}", self.catalog.render(&message));
        }
        self.motion = (0.0, 0.0);
    }

    fn record(&mut self, engine: &Gestures, micros: u64, events: Vec<Event>) {
        for event in events {
            let line = match event {
                Event::Motion { dx, dy } => {
                    self.tally.travel += (dx * dx + dy * dy).sqrt();
                    self.motion = (self.motion.0 + dx, self.motion.1 + dy);
                    None
                }
                Event::Press(button) => {
                    self.tally.clicks[button as usize] += 1;
                    Some(Msg::new("dev-gesture-press").sub("button", button_key(button)))
                }
                Event::Release(button) => Some(Msg::new("dev-gesture-release").sub("button", button_key(button))),
                Event::Scroll { dx, dy } => {
                    self.tally.scroll = (self.tally.scroll.0 + dx, self.tally.scroll.1 + dy);
                    None
                }
                Event::Swipe { fingers, phase, dx, dy } => {
                    if phase == Phase::Begin {
                        self.tally.swipes += 1;
                    }
                    (phase != Phase::Update).then(|| {
                        Msg::new("dev-gesture-swipe")
                            .num("fingers", fingers)
                            .sub("phase", phase_key(phase))
                            .arg("dx", format!("{dx:.1}"))
                            .arg("dy", format!("{dy:.1}"))
                    })
                }
                Event::Key { key, pressed } => {
                    self.tally.keys += usize::from(pressed);
                    pressed.then(|| Msg::new("dev-gesture-key").arg("key", key.name()))
                }
                Event::Pen { .. } => {
                    self.tally.pens += 1;
                    None
                }
                Event::PenAway => None,
                Event::Touch { fingers, .. } => {
                    self.tally.handed += 1;
                    Some(Msg::new("dev-gesture-touch").num("fingers", fingers))
                }
                Event::Notice(notice) => {
                    self.tally.notices += 1;
                    Some(elantp_linux::notice(engine, notice))
                }
            };
            if let Some(message) = line.filter(|_| !self.quiet) {
                self.flush_motion();
                let at = Msg::new("dev-gesture-at").arg("time", format!("{:.1}", micros as f64 / 1000.0)).nest("event", message);
                println!("{}", self.catalog.render(&at));
            }
        }
    }
}

fn gestures(catalog: &Catalog, options: &[&str]) -> Result<(), BoxError> {
    let (mut quiet, mut native, mut settings, mut recording) = (false, false, None, None);
    let mut rest = options.iter();
    while let Some(option) = rest.next() {
        match *option {
            "--summary" => quiet = true,
            "--native" => native = true,
            "--config" => settings = Some(PathBuf::from(rest.next().copied().unwrap_or_default())),
            other if !other.starts_with("--") && recording.is_none() => recording = Some(PathBuf::from(other)),
            other => return Err(Fail::new("dev-bad-option").arg("value", other).into()),
        }
    }
    let Some(path) = recording else {
        println!("{}", catalog.text("dev-usage"));
        return Ok(());
    };
    let loaded = match settings {
        Some(settings) => config::load(&settings)?,
        None => Loaded::default(),
    };
    let file = File::open(&path).map_err(|e| Fail::new("dev-read-failed").arg("file", path.display()).arg("detail", e))?;
    let recorded =
        recording::read(&mut BufReader::new(file)).map_err(|e| Fail::new("dev-bad-recording").arg("file", path.display()).arg("detail", format!("{e:?}")))?;
    let layout = layout(&recorded.descriptor)?;
    let mut assembler = Assembler::default();
    let mut engine = Gestures::with_symbols(loaded.config, surface(&layout), &loaded.symbols);
    if native {
        engine = engine.native();
    }
    let mut replay = Replay { catalog, quiet, tally: Tally::default(), motion: (0.0, 0.0) };
    let mut ticks = 0usize;
    let mut catch_up = |engine: &mut Gestures, replay: &mut Replay, until: u64| {
        while let Some(at) = engine.deadline().filter(|at| *at <= until && ticks < TICK_LIMIT) {
            ticks += 1;
            let events = engine.tick(at);
            replay.record(engine, at, events);
        }
    };
    let mut last = 0u64;
    let mut pressed = false;
    for (micros, bytes) in &recorded.reports {
        catch_up(&mut engine, &mut replay, micros.saturating_sub(1));
        last = *micros;
        let Some(frame) = layout.decode(bytes).ok().and_then(|report| assembler.push(report)) else { continue };
        let present = touches(&layout, &frame);
        let count = present.len();
        let events = engine.feed(&Input { time: *micros, button: frame.button, touches: present });
        if count > 0 {
            replay.tally.touched += 1;
            replay.tally.hidden += usize::from(engine.visible().len() < count);
        }
        if frame.button && !pressed {
            replay.tally.presses += 1;
            replay.tally.passed += usize::from(!engine.hidden());
        }
        pressed = frame.button;
        replay.record(&engine, *micros, events);
    }
    catch_up(&mut engine, &mut replay, last.saturating_add(REPLAY_TAIL));
    replay.flush_motion();
    let tally = &replay.tally;
    println!(
        "{}",
        catalog.render(
            &Msg::new("dev-gesture-summary")
                .num("left", tally.clicks[Button::Left as usize])
                .num("right", tally.clicks[Button::Right as usize])
                .num("middle", tally.clicks[Button::Middle as usize])
                .arg("travel", format!("{:.0}", tally.travel))
                .arg("scroll-x", format!("{:.0}", tally.scroll.0))
                .arg("scroll-y", format!("{:.0}", tally.scroll.1))
                .num("swipes", tally.swipes)
        )
    );
    if native {
        let native = Msg::new("dev-gesture-native")
            .num("frames", tally.touched)
            .num("hidden", tally.hidden)
            .num("handed", tally.handed)
            .num("presses", tally.presses)
            .num("passed", tally.passed);
        println!("{}", catalog.render(&native));
    }
    if tally.keys + tally.notices + tally.pens > 0 {
        println!("{}", catalog.render(&Msg::new("dev-gesture-extras").num("keys", tally.keys).num("notices", tally.notices).num("pens", tally.pens)));
    }
    Ok(())
}

struct Quiet {
    saved: Option<libc::termios>,
}

impl Quiet {
    fn new() -> Self {
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut saved) } != 0 {
            return Quiet { saved: None };
        }
        let mut quiet = saved;
        quiet.c_lflag &= !libc::ECHO;
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &quiet) };
        Quiet { saved: Some(saved) }
    }
}

impl Drop for Quiet {
    fn drop(&mut self) {
        if let Some(saved) = self.saved {
            unsafe {
                libc::tcflush(libc::STDIN_FILENO, libc::TCIFLUSH);
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &saved);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    Go,
    Skip,
    Quit,
}

fn ask(catalog: &Catalog, key: &str) -> Answer {
    unsafe { libc::tcflush(libc::STDIN_FILENO, libc::TCIFLUSH) };
    print!("{} ", catalog.text(key));
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
        return Answer::Quit;
    }
    match line.trim() {
        "n" | "N" | "т" | "Т" => Answer::Skip,
        "q" | "Q" | "й" | "Й" => Answer::Quit,
        _ => Answer::Go,
    }
}

fn stamp() -> String {
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&now, &mut tm) };
    format!("{:04}-{:02}-{:02}-{:02}{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min)
}

fn make_dir(path: &Path) -> Result<(), BoxError> {
    let mut built = PathBuf::new();
    for part in path.components() {
        built.push(part);
        if !built.exists() {
            std::fs::create_dir(&built).map_err(|e| Fail::new("dev-write-failed").arg("file", built.display()).arg("detail", e))?;
            hand_over_path(&built)?;
        }
    }
    Ok(())
}

fn points(layout: &Layout, frame: &Frame) -> Vec<Point> {
    touches(layout, frame).into_iter().map(|t| Point { id: t.id, x: t.x, y: t.y, palm: t.palm }).collect()
}

struct Screen {
    lines: usize,
    enabled: bool,
}

impl Screen {
    fn show(&mut self, lines: &[String]) {
        if !self.enabled {
            return;
        }
        let mut out = std::io::stdout().lock();
        if self.lines > 0 {
            let _ = write!(out, "\x1b[{}A", self.lines);
        }
        for line in lines {
            let _ = writeln!(out, "\x1b[2K{line}");
        }
        let _ = out.flush();
        self.lines = lines.len();
    }
}

struct Pad<'a> {
    layout: &'a Layout,
    descriptor: &'a [u8],
    device: &'a Device,
    nodes: &'a [PathBuf],
}

fn exercise(catalog: &Catalog, pad: &Pad, path: &Path, scenario: &Scenario) -> Result<(Stats, bool), BoxError> {
    let Pad { layout, descriptor, device, nodes } = *pad;
    let grab = match evdev::grab(nodes) {
        Ok(grab) => Some(grab),
        Err(e) => {
            println!("{}", catalog.render(&Msg::new("session-grab-failed").arg("detail", e)));
            None
        }
    };
    let quiet = Quiet::new();
    STOP.store(false, Ordering::Relaxed);
    catch_interrupt();
    for left in (1..=3u64).rev() {
        if STOP.load(Ordering::Relaxed) {
            break;
        }
        print!("{} ", catalog.render(&Msg::new("session-countdown").num("seconds", left)));
        let _ = std::io::stdout().flush();
        std::thread::sleep(Duration::from_secs(1));
    }
    println!("{}", catalog.text("session-go"));
    let mut buffer = vec![0u8; MAX_REPORT];
    while let Some(length) = device.read(&mut buffer)? {
        if length == 0 {
            break;
        }
    }
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| Fail::new("dev-write-failed").arg("file", path.display()).arg("detail", e))?;
    hand_over(&file)?;
    let mut out = BufWriter::new(file);
    recording::header(&mut out, descriptor)?;
    let size = (layout.x.micrometres as f32 / 1000.0, layout.y.micrometres as f32 / 1000.0);
    let mut screen = Screen { lines: 0, enabled: unsafe { libc::isatty(libc::STDOUT_FILENO) } == 1 };
    let mut assembler = Assembler::default();
    let mut stats = Stats::default();
    let (mut current, mut button) = (Vec::new(), false);
    let limit = Duration::from_secs(scenario.seconds);
    let start = Instant::now();
    let mut drawn = start - Duration::from_secs(1);
    while !STOP.load(Ordering::Relaxed) && start.elapsed() < limit {
        if device.wait(Duration::from_millis(50))? {
            while let Some(length) = device.read(&mut buffer)? {
                if length == 0 {
                    break;
                }
                let micros = u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX);
                recording::report(&mut out, micros, &buffer[..length])?;
                if let Some(frame) = layout.decode(&buffer[..length]).ok().and_then(|report| assembler.push(report)) {
                    current = points(layout, &frame);
                    button = frame.button;
                    stats.add(micros, button, &current);
                }
            }
        }
        if drawn.elapsed() >= Duration::from_millis(100) {
            drawn = Instant::now();
            let left = limit.saturating_sub(start.elapsed()).as_secs() + 1;
            let fingers = current.iter().filter(|p| !p.palm).count();
            let mut lines = vec![catalog.render(&Msg::new("session-recording").num("seconds", left).num("fingers", fingers))];
            lines.extend(session::draw(44, 11, size, &current, button));
            screen.show(&lines);
        }
    }
    out.flush()?;
    let interrupted = STOP.load(Ordering::Relaxed);
    release_interrupt();
    drop(quiet);
    drop(grab);
    Ok((stats, interrupted))
}

fn run_session(catalog: &Catalog, dir: Option<PathBuf>, only: &[&str]) -> Result<(), BoxError> {
    let names: Vec<&str> = SCENARIOS.iter().map(|s| s.name).collect();
    if let Some(unknown) = only.iter().find(|name| !names.contains(name)) {
        return Err(Fail::new("session-unknown").arg("name", unknown).arg("names", names.join(", ")).into());
    }
    let chosen: Vec<&Scenario> = SCENARIOS.iter().filter(|s| only.is_empty() || only.contains(&s.name)).collect();
    let found = found()?;
    let layout = layout(&found.descriptor)?;
    let device = open(&found)?;
    let nodes = evdev::nodes(&evdev::hid_prefix(found.bus, found.vendor, found.product))?;
    let dir = dir.unwrap_or_else(|| Path::new("captures/touchpad").join(stamp()));
    println!("{}", catalog.render(&Msg::new("session-intro").num("count", chosen.len())));
    println!("{}", catalog.text("session-intro-grab"));
    println!("{}", catalog.text("session-intro-keys"));
    if ask(catalog, "session-start") == Answer::Quit {
        return Ok(());
    }
    make_dir(&dir)?;
    let mut done: Vec<(&str, bool)> = Vec::new();
    'steps: for (number, scenario) in chosen.iter().enumerate() {
        loop {
            println!();
            println!(
                "{}",
                catalog.render(
                    &Msg::new("session-step")
                        .num("number", number + 1)
                        .num("count", chosen.len())
                        .sub("title", scenario.title)
                        .num("seconds", scenario.seconds)
                )
            );
            println!("{}", catalog.text(scenario.how));
            match ask(catalog, "session-ask") {
                Answer::Skip => continue 'steps,
                Answer::Quit => break 'steps,
                Answer::Go => {}
            }
            let path = dir.join(format!("{}.tprec", scenario.name));
            let pad = Pad { layout: &layout, descriptor: &found.descriptor, device: &device, nodes: &nodes };
            let (stats, interrupted) = exercise(catalog, &pad, &path, scenario)?;
            println!(
                "{}",
                catalog.render(
                    &Msg::new("session-result")
                        .num("frames", stats.frames)
                        .num("most", stats.most)
                        .num("taps", stats.taps)
                        .num("multi", stats.multi_taps)
                        .num("presses", stats.presses)
                )
            );
            let good = stats.done(scenario.expect);
            if interrupted {
                println!("{}", catalog.text("session-interrupted"));
                done.push((scenario.name, good));
                break 'steps;
            }
            if good {
                done.push((scenario.name, true));
                continue 'steps;
            }
            println!("{}", catalog.text(scenario.hint));
            match ask(catalog, "session-again") {
                Answer::Go => continue,
                Answer::Skip => {
                    done.push((scenario.name, false));
                    continue 'steps;
                }
                Answer::Quit => {
                    done.push((scenario.name, false));
                    break 'steps;
                }
            }
        }
    }
    println!();
    for (name, good) in &done {
        println!(
            "{}",
            catalog.render(&Msg::new("session-summary-line").arg("name", name).sub("state", if *good { "session-state-good" } else { "session-state-weak" }))
        );
    }
    println!("{}", catalog.render(&Msg::new("session-saved").arg("dir", dir.display())));
    println!("{}", catalog.text("session-next"));
    Ok(())
}

fn button_code(button: Button) -> u16 {
    match button {
        Button::Left => uinput::BTN_LEFT,
        Button::Right => uinput::BTN_RIGHT,
        Button::Middle => uinput::BTN_MIDDLE,
    }
}

fn micros(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

struct Outputs {
    pointer: uinput::Pointer,
    keyboard: uinput::Keyboard,
    pen: Option<uinput::Pen>,
    pen_failed: bool,
    surface: Surface,
}

impl Outputs {
    fn create(surface: Surface) -> Result<Self, BoxError> {
        let failed = |e: std::io::Error| Fail::new("dev-uinput-failed").arg("detail", e);
        Ok(Outputs {
            pointer: uinput::Pointer::create("elantp gestus").map_err(failed)?,
            keyboard: uinput::Keyboard::create("elantp gestus keys", gestus::Key::ALL.iter().map(|key| keys::code(*key))).map_err(failed)?,
            pen: None,
            pen_failed: false,
            surface,
        })
    }

    fn pen(&mut self, catalog: &Catalog) -> Option<&mut uinput::Pen> {
        if self.pen.is_none() && !self.pen_failed {
            match uinput::Pen::create("elantp gestus pen", self.surface.width, self.surface.height) {
                Ok(pen) => self.pen = Some(pen),
                Err(e) => {
                    self.pen_failed = true;
                    eprintln!("{}", catalog.render(&Msg::new("dev-pen-failed").arg("detail", e)));
                }
            }
        }
        self.pen.as_mut()
    }

    fn deliver(&mut self, catalog: &Catalog, engine: &Gestures, events: Vec<Event>) -> Result<(), BoxError> {
        for event in events {
            match event {
                Event::Motion { dx, dy } => self.pointer.motion(dx, dy)?,
                Event::Press(button) => self.pointer.button(button_code(button), true)?,
                Event::Release(button) => self.pointer.button(button_code(button), false)?,
                Event::Scroll { dx, dy } => self.pointer.scroll(dx, dy, SCROLL_PER_NOTCH)?,
                Event::Key { key, pressed } => self.keyboard.key(keys::code(key), pressed)?,
                Event::Pen { x, y, down } => {
                    if let Some(pen) = self.pen(catalog) {
                        pen.place(x, y, down)?;
                    }
                }
                Event::PenAway => {
                    if let Some(pen) = self.pen.as_mut() {
                        pen.away()?;
                    }
                }
                Event::Notice(notice) => println!("{}", catalog.render(&elantp_linux::notice(engine, notice))),
                Event::Swipe { .. } | Event::Touch { .. } => {}
            }
        }
        Ok(())
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

#[derive(Debug, Clone, Copy)]
struct Tweaks {
    natural: bool,
    speed: f32,
}

fn build(loaded: &Loaded, tweaks: Tweaks, surface: Surface) -> Gestures {
    let mut settings = loaded.config.clone();
    settings.pointer.natural_scroll |= tweaks.natural;
    settings.pointer.speed *= tweaks.speed;
    Gestures::with_symbols(settings, surface, &loaded.symbols)
}

fn watch_typing(catalog: &Catalog, keyboards: &mut Keyboards, loaded: &Loaded) {
    if !loaded.config.typing.enabled {
        keyboards.clear();
        return;
    }
    keyboards.refresh();
    let message = if keyboards.count() == 0 { Msg::new("dev-typing-none") } else { Msg::new("dev-typing-keyboards").num("count", keyboards.count()) };
    println!("{}", catalog.render(&message));
}

fn drive(catalog: &Catalog, options: &[&str]) -> Result<(), BoxError> {
    let mut tweaks = Tweaks { natural: false, speed: 1.0 };
    let mut chosen: Option<PathBuf> = None;
    let mut rest = options.iter();
    while let Some(option) = rest.next() {
        match *option {
            "--natural" => tweaks.natural = true,
            "--speed" => {
                let value = rest.next().copied().unwrap_or_default();
                tweaks.speed = value.parse().ok().filter(|f: &f32| (0.1..=5.0).contains(f)).ok_or_else(|| Fail::new("dev-bad-speed").arg("value", value))?;
            }
            "--config" => chosen = Some(PathBuf::from(rest.next().copied().unwrap_or_default())),
            other => return Err(Fail::new("dev-bad-option").arg("value", other).into()),
        }
    }
    let explicit = chosen.is_some();
    let path = chosen.unwrap_or_else(|| PathBuf::from(config::SYSTEM));
    let loaded = if explicit || path.exists() {
        let loaded = config::load(&path)?;
        println!("{}", catalog.render(&Msg::new("dev-config-used").arg("file", path.display())));
        loaded
    } else {
        println!("{}", catalog.render(&Msg::new("dev-config-default").arg("file", path.display())));
        Loaded::default()
    };
    let mut stamp = config::stamp(&path);
    let found = found()?;
    let layout = layout(&found.descriptor)?;
    let device = open(&found)?;
    let nodes = evdev::nodes(&evdev::hid_prefix(found.bus, found.vendor, found.product))?;
    let surface = surface(&layout);
    let mut outputs = Outputs::create(surface)?;
    let grab = evdev::grab(&nodes).map_err(|e| Fail::new("dev-grab-failed").arg("detail", e))?;
    catch_interrupt();
    println!("{}", catalog.text("dev-drive-start"));
    let mut engine = build(&loaded, tweaks, surface);
    let mut keyboards = Keyboards::default();
    watch_typing(catalog, &mut keyboards, &loaded);
    let mut assembler = Assembler::default();
    let mut buffer = vec![0u8; MAX_REPORT];
    let started = Instant::now();
    let mut checked = Instant::now();
    while !STOP.load(Ordering::Relaxed) {
        let wait = engine.deadline().map_or(POLL, |at| Duration::from_millis(at.saturating_sub(micros(started)).div_ceil(1000)).min(POLL));
        let mut fds = vec![device.raw()];
        fds.extend(keyboards.fds());
        let ready = keyboard::wait(&fds, wait)?;
        if ready[0] & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(std::io::Error::from(ErrorKind::BrokenPipe).into());
        }
        if ready[1..].iter().any(|events| *events != 0) && keyboards.typed() {
            engine.typed(micros(started));
        }
        if ready[0] & libc::POLLIN != 0 {
            while let Some(length) = device.read(&mut buffer)? {
                if length == 0 {
                    break;
                }
                let Some(frame) = layout.decode(&buffer[..length]).ok().and_then(|report| assembler.push(report)) else { continue };
                let events = engine.feed(&Input { time: micros(started), button: frame.button, touches: touches(&layout, &frame) });
                outputs.deliver(catalog, &engine, events)?;
            }
        }
        let now = micros(started);
        if engine.deadline().is_some_and(|at| at <= now) {
            let events = engine.tick(now);
            outputs.deliver(catalog, &engine, events)?;
        }
        if checked.elapsed() >= RELOAD {
            checked = Instant::now();
            let fresh = config::stamp(&path);
            if fresh != stamp {
                stamp = fresh;
                let reloaded = if path.exists() { config::load(&path) } else { Ok(Loaded::default()) };
                match reloaded {
                    Ok(loaded) => {
                        outputs.let_go()?;
                        engine = build(&loaded, tweaks, surface);
                        watch_typing(catalog, &mut keyboards, &loaded);
                        println!("{}", catalog.render(&Msg::new("dev-config-reloaded").arg("file", path.display())));
                    }
                    Err(e) => {
                        eprintln!("{}", catalog.render(&as_msg(&e, "dev-error")));
                        eprintln!("{}", catalog.text("dev-config-kept"));
                    }
                }
            }
        }
    }
    release_interrupt();
    outputs.let_go()?;
    drop(grab);
    drop(outputs);
    println!("{}", catalog.text("dev-drive-stop"));
    Ok(())
}

fn learn(catalog: &Catalog, options: &[&str]) -> Result<(), BoxError> {
    let (mut name, mut target, mut times) = (None, None, LEARN_TIMES);
    let mut rest = options.iter();
    while let Some(option) = rest.next() {
        match *option {
            "--file" => target = Some(PathBuf::from(rest.next().copied().unwrap_or_default())),
            "--times" => {
                let value = rest.next().copied().unwrap_or_default();
                times = value.parse().ok().filter(|n| (1..=MAX_LEARN_TIMES).contains(n)).ok_or_else(|| Fail::new("dev-learn-bad-times").arg("value", value))?;
            }
            other if !other.starts_with("--") && name.is_none() => name = Some(other.to_string()),
            other => return Err(Fail::new("dev-bad-option").arg("value", other).into()),
        }
    }
    let name = name.unwrap_or_default();
    if !gestus::config::valid_name(&name) {
        return Err(Fail::new("dev-learn-bad-name").arg("value", drv_i18n::printable(&name)).into());
    }
    let target = target.unwrap_or_else(|| config::symbols_beside(Path::new(config::SYSTEM)));
    let found = found()?;
    let layout = layout(&found.descriptor)?;
    let device = open(&found)?;
    let nodes = evdev::nodes(&evdev::hid_prefix(found.bus, found.vendor, found.product))?;
    let grab = evdev::grab(&nodes).map_err(|e| Fail::new("dev-grab-failed").arg("detail", e))?;
    catch_interrupt();
    println!("{}", catalog.render(&Msg::new("dev-learn-start").arg("name", &name).num("times", times).arg("file", target.display())));
    println!("{}", catalog.render(&Msg::new("dev-learn-draw").arg("name", &name).num("done", 0).num("times", times)));
    let mut assembler = Assembler::default();
    let mut samples: Vec<Vec<(f32, f32)>> = Vec::new();
    let mut stroke: Vec<(f32, f32)> = Vec::new();
    let mut most = 0usize;
    pump(&device, None, |_, bytes| {
        let Some(frame) = layout.decode(bytes).ok().and_then(|report| assembler.push(report)) else { return Ok(()) };
        let fingers: Vec<Touch> = touches(&layout, &frame).into_iter().filter(|touch| !touch.palm).collect();
        if let Some(first) = fingers.first() {
            most = most.max(fingers.len());
            stroke.push((first.x, first.y));
            return Ok(());
        }
        if stroke.is_empty() {
            return Ok(());
        }
        let finished = std::mem::take(&mut stroke);
        let length: f32 = finished.windows(2).map(|pair| ((pair[1].0 - pair[0].0).powi(2) + (pair[1].1 - pair[0].1).powi(2)).sqrt()).sum();
        if std::mem::take(&mut most) > 1 {
            println!("{}", catalog.text("dev-learn-one-finger"));
        } else if length < SHORTEST_STROKE {
            println!("{}", catalog.text("dev-learn-short"));
        } else if let Some(points) = gestus::normalize(&finished) {
            samples.push(points);
            if samples.len() >= times {
                STOP.store(true, Ordering::Relaxed);
            } else {
                println!("{}", catalog.render(&Msg::new("dev-learn-draw").arg("name", &name).num("done", samples.len()).num("times", times)));
            }
        }
        Ok(())
    })?;
    release_interrupt();
    drop(grab);
    if samples.len() < times {
        println!("{}", catalog.text("dev-learn-stopped"));
        return Ok(());
    }
    let mut text = if target.exists() {
        std::fs::read_to_string(&target).map_err(|e| Fail::new("dev-read-failed").arg("file", target.display()).arg("detail", e))?
    } else {
        String::new()
    };
    gestus::config::templates(&text).map_err(|problem| config::invalid(&target, problem))?;
    for points in samples {
        let template = gestus::Template { name: name.clone(), points };
        if let Some(entry) = gestus::config::entry(&template) {
            if !text.is_empty() && !text.ends_with("\n\n") {
                text.push('\n');
            }
            text.push_str(&entry);
        }
    }
    let written = |e: std::io::Error| Fail::new("dev-write-failed").arg("file", target.display()).arg("detail", e);
    if let Some(parent) = target.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(written)?;
    }
    let draft = target.with_extension("toml.new");
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .open(&draft)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .map_err(written)?;
    std::fs::rename(&draft, &target).map_err(written)?;
    println!("{}", catalog.render(&Msg::new("dev-learn-saved").arg("name", &name).num("count", times).arg("file", target.display())));
    Ok(())
}

fn live(catalog: &Catalog) -> Result<(), BoxError> {
    let found = found()?;
    let layout = layout(&found.descriptor)?;
    let device = open(&found)?;
    catch_interrupt();
    println!("{}", catalog.render(&Msg::new("dev-live")));
    let mut assembler = Assembler::default();
    pump(&device, None, |micros, bytes| {
        if let Ok(report) = layout.decode(bytes)
            && let Some(frame) = assembler.push(report)
        {
            show_frame(catalog, &layout, micros, &frame);
        }
        Ok(())
    })?;
    Ok(())
}

fn main() -> ExitCode {
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    let catalog = elantp_linux::catalog();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match words.as_slice() {
        ["info"] => info(&catalog),
        ["record", file] => record(&catalog, Path::new(file), None),
        ["record", file, seconds] => match seconds.parse::<u64>() {
            Ok(seconds) if seconds > 0 => record(&catalog, Path::new(file), Some(seconds)),
            _ => Err(Fail::new("dev-bad-seconds").arg("value", seconds).into()),
        },
        ["show", file] => show(&catalog, Path::new(file)),
        ["gestures", options @ ..] => gestures(&catalog, options),
        ["learn", options @ ..] => learn(&catalog, options),
        ["live"] => live(&catalog),
        ["drive", options @ ..] => drive(&catalog, options),
        ["session", "--dir", dir, only @ ..] => run_session(&catalog, Some(PathBuf::from(dir)), only),
        ["session", only @ ..] => run_session(&catalog, None, only),
        _ => {
            println!("{}", catalog.text("dev-usage"));
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{}", catalog.render(&as_msg(e.as_ref(), "dev-error")));
            ExitCode::FAILURE
        }
    }
}
