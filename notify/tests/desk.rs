use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use drv_i18n::{Catalog, Msg};
use zbus::blocking::Connection;
use zbus::blocking::connection::Builder;
use zbus::zvariant::{OwnedValue, Value};

const PATIENCE: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, PartialEq)]
struct Shown {
    app: String,
    replaces: u32,
    icon: String,
    summary: String,
    body: String,
    expires: i32,
    transient: bool,
}

type Shared = Arc<Mutex<Vec<Shown>>>;

fn lock(seen: &Shared) -> MutexGuard<'_, Vec<Shown>> {
    seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct Board(Shared);

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Board {
    fn get_capabilities(&self) -> Vec<String> {
        vec!["body".to_string()]
    }

    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app: String,
        replaces: u32,
        icon: String,
        summary: String,
        body: String,
        _actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expires: i32,
    ) -> u32 {
        let transient = hints.get("transient").is_some_and(|hint| **hint == Value::Bool(true));
        let mut seen = lock(&self.0);
        seen.push(Shown { app, replaces, icon, summary, body, expires, transient });
        seen.len() as u32
    }

    fn close_notification(&self, _id: u32) {}
}

fn eventually<T>(what: &str, mut ready: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(found) = ready() {
            return found;
        }
        assert!(started.elapsed() < PATIENCE, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct Stand {
    bus: Child,
    agents: Vec<Child>,
    address: String,
    dir: PathBuf,
    socket: PathBuf,
    seen: Shared,
    _board: Connection,
}

impl Stand {
    fn start(tag: &str) -> Stand {
        let spawned = Command::new("dbus-daemon").args(["--session", "--nofork", "--print-address"]).stdout(Stdio::piped()).stderr(Stdio::null()).spawn();
        let mut bus = spawned.expect("the agent tests need dbus-daemon");
        let mut address = String::new();
        BufReader::new(bus.stdout.take().unwrap()).read_line(&mut address).unwrap();
        let address = address.trim_end().to_string();
        let seen = Shared::default();
        let board = Builder::address(address.as_str())
            .unwrap()
            .name("org.freedesktop.Notifications")
            .unwrap()
            .serve_at("/org/freedesktop/Notifications", Board(Arc::clone(&seen)))
            .unwrap()
            .build()
            .unwrap();
        let dir = std::env::temp_dir().join(format!("elantp-notify-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("notices");
        Stand { bus, agents: Vec::new(), address, dir, socket, seen, _board: board }
    }

    fn agent(&mut self) -> usize {
        let agent = Command::new(env!("CARGO_BIN_EXE_elantp-notify"))
            .env("DBUS_SESSION_BUS_ADDRESS", &self.address)
            .env("ELANTP_NOTICES", &self.socket)
            .env("LC_ALL", "en_US.UTF-8")
            .env_remove("LANGUAGE")
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        self.agents.push(agent);
        self.agents.len() - 1
    }

    fn shown(&self) -> Vec<Shown> {
        lock(&self.seen).clone()
    }

    fn ended(&mut self, agent: usize) -> ExitStatus {
        let child = &mut self.agents[agent];
        eventually("the agent to leave", || child.try_wait().unwrap())
    }
}

impl Drop for Stand {
    fn drop(&mut self) {
        for agent in &mut self.agents {
            let _ = agent.kill();
            let _ = agent.wait();
        }
        let _ = self.bus.kill();
        let _ = self.bus.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn service(socket: &Path) -> UnixListener {
    let listener = UnixListener::bind(socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    listener
}

fn connected(listener: &UnixListener) -> UnixStream {
    let (stream, _) = eventually("the agent to connect", || listener.accept().ok());
    stream.set_nonblocking(false).unwrap();
    stream
}

fn english() -> Catalog {
    Catalog::for_languages(&elantp_linux::LOCALES, &["en".to_string()])
}

#[test]
fn notices_become_notifications_that_replace_each_other_and_junk_is_dropped() {
    let mut stand = Stand::start("replace");
    let listener = service(&stand.socket);
    stand.agent();
    let mut stream = connected(&listener);
    for line in ["gesture-numpad-on", "dev-usage", "gesture-symbol ../../etc", "gesture-numpad-on now", "gesture-symbol circle", "gesture-tablet-on"] {
        stream.write_all(format!("{line}\n").as_bytes()).unwrap();
    }
    eventually("three notifications", || (stand.shown().len() >= 3).then_some(()));
    std::thread::sleep(Duration::from_millis(300));
    let shown = stand.shown();
    let texts = english();
    let expected = [Msg::new("gesture-numpad-on"), Msg::new("gesture-symbol").arg("name", "circle"), Msg::new("gesture-tablet-on")];
    assert_eq!(shown.iter().map(|card| card.summary.clone()).collect::<Vec<_>>(), expected.iter().map(|message| texts.render(message)).collect::<Vec<_>>());
    assert_eq!(shown.iter().map(|card| card.replaces).collect::<Vec<_>>(), [0, 1, 2]);
    for card in &shown {
        assert_eq!(
            (card.app.as_str(), card.icon.as_str(), card.body.as_str(), card.expires, card.transient),
            (texts.text("notify-app").as_str(), "input-touchpad", "", 2500, true)
        );
    }
}

#[test]
fn the_agent_comes_back_after_the_service_restarts_and_runs_once_per_session() {
    let mut stand = Stand::start("restart");
    let listener = service(&stand.socket);
    let first = stand.agent();
    drop(connected(&listener));
    drop(listener);
    std::fs::remove_file(&stand.socket).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let listener = service(&stand.socket);
    let mut stream = connected(&listener);
    stream.write_all(b"gesture-touchpad-off\n").unwrap();
    eventually("a notification after the restart", || (stand.shown().len() == 1).then_some(()));
    assert_eq!(stand.shown()[0].summary, english().render(&Msg::new("gesture-touchpad-off")));
    let second = stand.agent();
    assert!(stand.ended(second).success());
    assert!(stand.agents[first].try_wait().unwrap().is_none());
}

#[test]
fn a_line_too_long_ends_the_connection_and_shows_nothing() {
    let mut stand = Stand::start("long");
    let listener = service(&stand.socket);
    stand.agent();
    let mut stream = connected(&listener);
    stream.write_all(format!("gesture-symbol {}\n", "x".repeat(elantp_linux::notices::MAX_LINE)).as_bytes()).unwrap();
    let mut again = connected(&listener);
    again.write_all(b"gesture-numpad-off\n").unwrap();
    eventually("the notification after the long line", || (stand.shown().len() == 1).then_some(()));
    assert_eq!(stand.shown()[0].summary, english().render(&Msg::new("gesture-numpad-off")));
}
