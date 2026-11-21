use std::collections::HashMap;
use std::io::{BufRead, BufReader, ErrorKind, Read};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use drv_i18n::{Catalog, Fail, Msg, as_msg};
use elantp_linux::notices::{self, MAX_LINE};
use zbus::blocking::Connection;
use zbus::fdo::RequestNameFlags;
use zbus::zvariant::Value;

type BoxError = Box<dyn std::error::Error>;

const OWN_NAME: &str = "org.elantp.Notify";
const NOTIFICATIONS: &str = "org.freedesktop.Notifications";
const NOTIFICATIONS_PATH: &str = "/org/freedesktop/Notifications";
const BUS: &str = "org.freedesktop.DBus";
const BUS_PATH: &str = "/org/freedesktop/DBus";
const PEER: &str = "org.freedesktop.DBus.Peer";
const ICON: &str = "input-touchpad";
const SHOWN_FOR: i32 = 2500;
const NORMAL: u8 = 1;
const ROOT: u32 = 0;
const BUS_CALL_TIMEOUT: Duration = Duration::from_secs(10);
const QUIET_CHECK: Duration = Duration::from_secs(30);
const RETRIES: [Duration; 4] = [Duration::from_millis(200), Duration::from_secs(1), Duration::from_secs(2), Duration::from_secs(5)];

struct Desk {
    bus: Connection,
    texts: Catalog,
    shown: u32,
}

impl Desk {
    fn open(texts: Catalog) -> Result<Option<Desk>, BoxError> {
        let unreachable = |e: zbus::Error| Fail::new("notify-no-bus").arg("detail", e);
        let bus = zbus::blocking::connection::Builder::session().map_err(unreachable)?.method_timeout(BUS_CALL_TIMEOUT).build().map_err(unreachable)?;
        match bus.request_name_with_flags(OWN_NAME, RequestNameFlags::DoNotQueue.into()) {
            Ok(_) => Ok(Some(Desk { bus, texts, shown: 0 })),
            Err(zbus::Error::NameTaken) => Ok(None),
            Err(e) => Err(unreachable(e).into()),
        }
    }

    fn alive(&self) -> bool {
        let pinged = self.bus.call_method(Some(BUS), BUS_PATH, Some(PEER), "Ping", &());
        !matches!(pinged, Err(zbus::Error::InputOutput(_)))
    }

    fn show(&mut self, notice: &Msg) {
        let (app, summary) = (self.texts.text("notify-app"), self.texts.render(notice));
        let hints: HashMap<&str, Value<'_>> = HashMap::from([("urgency", Value::U8(NORMAL)), ("transient", Value::Bool(true))]);
        let request = (app.as_str(), self.shown, ICON, summary.as_str(), "", Vec::<&str>::new(), hints, SHOWN_FOR);
        let reply = self.bus.call_method(Some(NOTIFICATIONS), NOTIFICATIONS_PATH, Some(NOTIFICATIONS), "Notify", &request);
        self.shown = reply.ok().and_then(|reply| reply.body().deserialize::<u32>().ok()).unwrap_or(0);
    }
}

fn trusted(stream: &UnixStream, own: u32) -> bool {
    notices::peer_uid(stream).is_ok_and(|peer| peer == ROOT || peer == own)
}

fn follow(socket: &Path, own: u32, desk: &mut Desk) -> bool {
    let Some(stream) = UnixStream::connect(socket).ok().filter(|stream| trusted(stream, own)) else { return false };
    if stream.set_read_timeout(Some(QUIET_CHECK)).is_err() {
        return false;
    }
    let mut reader = BufReader::new(stream);
    let (mut line, mut heard) = (Vec::new(), false);
    loop {
        let room = MAX_LINE.saturating_sub(line.len()) as u64;
        match (&mut reader).take(room).read_until(b'\n', &mut line) {
            Ok(0) => return heard,
            Ok(_) if line.last() == Some(&b'\n') => {
                heard = true;
                if let Some(notice) = std::str::from_utf8(&line).ok().and_then(notices::parse) {
                    desk.show(&notice);
                }
                line.clear();
            }
            Ok(_) if line.len() >= MAX_LINE => return heard,
            Ok(_) => {}
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => {}
            Err(_) => return heard,
        }
        if !desk.alive() {
            return heard;
        }
    }
}

fn run() -> Result<(), BoxError> {
    let socket = std::env::var_os("ELANTP_NOTICES").filter(|path| !path.is_empty()).map_or_else(|| PathBuf::from(notices::SOCKET), PathBuf::from);
    let own = unsafe { libc::getuid() };
    let Some(mut desk) = Desk::open(elantp_linux::catalog())? else { return Ok(()) };
    let mut fruitless = 0;
    while desk.alive() {
        fruitless = if follow(&socket, own, &mut desk) { 0 } else { fruitless + 1 };
        std::thread::sleep(RETRIES[fruitless.min(RETRIES.len() - 1)]);
    }
    Err(Fail::new("notify-bus-lost").into())
}

fn main() {
    if let Err(e) = run() {
        let texts = elantp_linux::catalog();
        eprintln!("{}", texts.render(&as_msg(e.as_ref(), "notify-error")));
        std::process::exit(1);
    }
}
