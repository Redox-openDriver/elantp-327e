use std::fs::File;
use std::io::{BufReader, BufWriter, ErrorKind, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use drv_i18n::{Catalog, Fail, Msg, as_msg};
use elantp_core::identity::{self, Unverified};
use elantp_core::report::ReportDescriptor;
use elantp_core::touchpad::{Assembler, Axis, Frame, Layout, PadType};
use elantp_linux::hidraw::{self, Device, Found, MAX_REPORT};
use elantp_linux::recording;
use gestus::{Button, Event, Gestures, Input, Phase, Touch};

type BoxError = Box<dyn std::error::Error>;

const POLL: Duration = Duration::from_millis(100);

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
}

fn gestures(catalog: &Catalog, path: &Path, quiet: bool) -> Result<(), BoxError> {
    let file = File::open(path).map_err(|e| Fail::new("dev-read-failed").arg("file", path.display()).arg("detail", e))?;
    let recorded =
        recording::read(&mut BufReader::new(file)).map_err(|e| Fail::new("dev-bad-recording").arg("file", path.display()).arg("detail", format!("{e:?}")))?;
    let layout = layout(&recorded.descriptor)?;
    let mut assembler = Assembler::default();
    let mut engine = Gestures::new(gestus::DEFAULT);
    let mut tally = Tally::default();
    let mut motion = (0.0f32, 0.0f32);
    for (micros, bytes) in &recorded.reports {
        let Some(frame) = layout.decode(bytes).ok().and_then(|report| assembler.push(report)) else { continue };
        let events = engine.feed(&Input { time: *micros, button: frame.button, touches: touches(&layout, &frame) });
        for event in events {
            let line = match event {
                Event::Motion { dx, dy } => {
                    tally.travel += (dx * dx + dy * dy).sqrt();
                    motion = (motion.0 + dx, motion.1 + dy);
                    None
                }
                Event::Press(button) => {
                    tally.clicks[button as usize] += 1;
                    Some(Msg::new("dev-gesture-press").sub("button", button_key(button)))
                }
                Event::Release(button) => Some(Msg::new("dev-gesture-release").sub("button", button_key(button))),
                Event::Scroll { dx, dy } => {
                    tally.scroll = (tally.scroll.0 + dx, tally.scroll.1 + dy);
                    None
                }
                Event::Swipe { fingers, phase, dx, dy } => {
                    if phase == Phase::Begin {
                        tally.swipes += 1;
                    }
                    (phase != Phase::Update).then(|| {
                        Msg::new("dev-gesture-swipe")
                            .num("fingers", fingers)
                            .sub("phase", phase_key(phase))
                            .arg("dx", format!("{dx:.1}"))
                            .arg("dy", format!("{dy:.1}"))
                    })
                }
            };
            if let Some(message) = line.filter(|_| !quiet) {
                if motion != (0.0, 0.0) {
                    println!("{}", catalog.render(&Msg::new("dev-gesture-motion").arg("dx", format!("{:.0}", motion.0)).arg("dy", format!("{:.0}", motion.1))));
                    motion = (0.0, 0.0);
                }
                println!("{}", catalog.render(&Msg::new("dev-gesture-at").arg("time", format!("{:.1}", *micros as f64 / 1000.0)).nest("event", message)));
            }
        }
    }
    if !quiet && motion != (0.0, 0.0) {
        println!("{}", catalog.render(&Msg::new("dev-gesture-motion").arg("dx", format!("{:.0}", motion.0)).arg("dy", format!("{:.0}", motion.1))));
    }
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
        ["gestures", file] => gestures(&catalog, Path::new(file), false),
        ["gestures", "--summary", file] => gestures(&catalog, Path::new(file), true),
        ["live"] => live(&catalog),
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
