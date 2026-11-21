use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use drv_i18n::Msg;
use gestus::{Gestures, Notice};

pub const SOCKET: &str = "/run/elantp/notices";
pub const MAX_LINE: usize = 128;
const MOST_LISTENERS: usize = 16;
const SYMBOL: &str = "gesture-symbol";

pub fn line(gestures: &Gestures, notice: Notice) -> String {
    match notice {
        Notice::Symbol(index) => format!("{SYMBOL} {}", gestures.symbol(index).unwrap_or_default()),
        other => other.key().to_string(),
    }
}

pub fn parse(line: &str) -> Option<Msg> {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let (key, name) = line.split_once(' ').map_or((line, None), |(key, name)| (key, Some(name)));
    let key = Notice::KEYS.into_iter().find(|known| *known == key)?;
    match name {
        Some(name) if key == SYMBOL && gestus::config::valid_name(name) => Some(Msg::new(key).arg("name", name)),
        None if key != SYMBOL => Some(Msg::new(key)),
        _ => None,
    }
}

pub fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let mut credentials = libc::ucred { pid: 0, uid: u32::MAX, gid: u32::MAX };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let asked = unsafe { libc::getsockopt(stream.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, (&raw mut credentials).cast(), &mut length) };
    if asked != 0 {
        return Err(io::Error::last_os_error());
    }
    if length as usize != std::mem::size_of::<libc::ucred>() || credentials.uid == u32::MAX {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(credentials.uid)
}

pub struct Notices {
    path: PathBuf,
    listener: UnixListener,
    listeners: Vec<UnixStream>,
}

impl Notices {
    pub fn open(path: &Path) -> io::Result<Notices> {
        if let Some(dir) = path.parent().filter(|dir| !dir.exists()) {
            std::fs::DirBuilder::new().mode(0o755).create(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))?;
        }
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))?;
        listener.set_nonblocking(true)?;
        Ok(Notices { path: path.to_path_buf(), listener, listeners: Vec::new() })
    }

    pub fn send(&mut self, line: &str) {
        while let Ok((stream, _)) = self.listener.accept() {
            if self.listeners.len() >= MOST_LISTENERS {
                self.listeners.remove(0);
            }
            self.listeners.push(stream);
        }
        let bytes = format!("{line}\n").into_bytes();
        if bytes.len() <= MAX_LINE {
            self.listeners.retain(|stream| sent(stream, &bytes));
        }
    }
}

impl Drop for Notices {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn sent(stream: &UnixStream, bytes: &[u8]) -> bool {
    let written = unsafe { libc::send(stream.as_raw_fd(), bytes.as_ptr().cast(), bytes.len(), libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT) };
    usize::try_from(written).is_ok_and(|written| written == bytes.len())
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::time::Duration;

    use super::*;

    #[test]
    fn every_notice_goes_through_a_line_and_nothing_else_does() {
        let mut config = gestus::Config::default();
        config.symbols.enabled = true;
        let gestures = Gestures::new(config, gestus::Surface { width: 117.0, height: 72.0 });
        let circle = (0..256).find(|index| gestures.symbol(*index) == Some("circle")).unwrap();
        let notices = [
            Notice::Touchpad(true),
            Notice::Touchpad(false),
            Notice::Numpad(true),
            Notice::Numpad(false),
            Notice::Tablet(true),
            Notice::Tablet(false),
            Notice::Drawing,
            Notice::Symbol(circle),
            Notice::Unrecognized,
            Notice::Cancelled,
        ];
        for notice in notices {
            let message = parse(&line(&gestures, notice)).unwrap();
            assert_eq!(message.key, notice.key());
        }
        assert_eq!(line(&gestures, Notice::Symbol(circle)), "gesture-symbol circle");
        let catalog = crate::catalog();
        assert!(catalog.render(&parse("gesture-symbol circle\n").unwrap()).contains("circle"));
        for junk in
            ["", "\n", "gesture-symbol", "gesture-symbol ", "gesture-symbol a b", "gesture-symbol ../x", "gesture-numpad-on x", "dev-usage", "service-error"]
        {
            assert!(parse(junk).is_none(), "{junk:?}");
        }
        assert!(parse(&format!("gesture-symbol {}", "x".repeat(200))).is_none());
    }

    #[test]
    fn listeners_get_every_line_and_those_who_left_are_dropped_quietly() {
        let dir = std::env::temp_dir().join(format!("elantp-notices-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("run").join("notices");
        std::fs::create_dir_all(&dir).unwrap();
        let mut notices = Notices::open(&path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o666);
        assert_eq!(std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o755);
        let staying = UnixStream::connect(&path).unwrap();
        let leaving = UnixStream::connect(&path).unwrap();
        assert_eq!(peer_uid(&staying).unwrap(), unsafe { libc::getuid() });
        notices.send("gesture-numpad-on");
        drop(leaving);
        notices.send("gesture-numpad-off");
        notices.send(&"x".repeat(MAX_LINE));
        notices.send("gesture-tablet-on");
        staying.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let lines: Vec<String> = BufReader::new(&staying).lines().take(3).map(Result::unwrap).collect();
        assert_eq!(lines, ["gesture-numpad-on", "gesture-numpad-off", "gesture-tablet-on"]);
        assert_eq!(notices.listeners.len(), 1);
        let crowd: Vec<UnixStream> = (0..MOST_LISTENERS + 2).map(|_| UnixStream::connect(&path).unwrap()).collect();
        notices.send("gesture-drawing");
        assert_eq!(notices.listeners.len(), MOST_LISTENERS);
        drop(crowd);
        drop(notices);
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
