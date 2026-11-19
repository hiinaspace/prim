//! The room daemon as a child of the session: start it, notice when it is up or gone,
//! stop it with a protocol request (not signals), adopt one that is already running.

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Child;
use std::time::{Duration, Instant};

use prim_client::chat::clip;
use prim_client::client::Client;
use prim_client::paths;
use prim_client::transport::UnixTransport;
use prim_client::wire::{ReplyJson, Request};

use crate::procs;

#[derive(Clone, Debug)]
pub enum RoomKind {
    Found { name: String },
    Join(String),
    Resume(String),
}

enum Phase {
    None,
    Starting,
    Up,
    Failed(String),
}

pub struct Room {
    phase: Phase,
    kind: &'static str,
    child: Option<Child>,
    started: Instant,
    /// Last time we connected to the socket just to see that it answers.
    probed: Instant,
}

pub fn alive(sock: &Path) -> bool {
    UnixStream::connect(sock).is_ok()
}

impl Room {
    pub fn new() -> Room {
        Room {
            phase: Phase::None,
            kind: "",
            child: None,
            started: Instant::now(),
            probed: Instant::now(),
        }
    }

    pub fn phase(&self) -> &'static str {
        match self.phase {
            Phase::None => "none",
            Phase::Starting => "starting",
            Phase::Up => "up",
            Phase::Failed(_) => "failed",
        }
    }
    pub fn error(&self) -> &str {
        match &self.phase {
            Phase::Failed(e) => e,
            _ => "",
        }
    }
    pub fn kind_name(&self) -> &'static str {
        self.kind
    }

    /// A daemon was already running when the session came up.
    pub fn adopt(&mut self, sock: &Path) {
        if alive(sock) {
            self.phase = Phase::Up;
            self.kind = "attached";
        }
    }

    pub fn start(&mut self, kind: RoomKind, extra: Vec<String>, sock: &Path) -> Result<(), String> {
        if alive(sock) || matches!(self.phase, Phase::Starting) {
            return Err("a room is already running; leave it first".into());
        }
        let mut args: Vec<String> = vec!["room".into()];
        self.kind = match &kind {
            RoomKind::Found { name } => {
                args.push("found".into());
                let n = clip(name.trim(), 64);
                if !n.is_empty() {
                    args.push(format!("--meta={n}"));
                }
                "found"
            }
            RoomKind::Join(t) => {
                args.extend(["join".into(), t.clone()]);
                "join"
            }
            RoomKind::Resume(h) => {
                args.extend(["resume".into(), h.clone()]);
                "resume"
            }
        };
        args.push(format!("--socket={}", sock.display()));
        args.push(format!("--rt-socket={}", paths::rt_socket(sock).display()));
        args.extend(extra);
        for var in ["PRIM_SESSION_DAEMON_ARGS", "PRIM_UI_DAEMON_ARGS"] {
            if let Ok(extra) = std::env::var(var) {
                args.extend(extra.split_whitespace().map(str::to_string));
                break;
            }
        }
        let child = procs::spawn(&args, "room.log").map_err(|e| format!("spawn prim room: {e}"))?;
        self.child = Some(child);
        self.phase = Phase::Starting;
        self.started = Instant::now();
        Ok(())
    }

    /// Notice the daemon coming up, exiting, or never starting.
    pub fn poll(&mut self, sock: &Path) {
        if let Some(c) = &mut self.child {
            if let Ok(Some(st)) = c.try_wait() {
                self.child = None;
                if !matches!(self.phase, Phase::Up) || !alive(sock) {
                    self.phase = if matches!(self.phase, Phase::Starting) {
                        Phase::Failed(format!("prim room exited ({st}): {}", procs::last_log_line("room.log")))
                    } else {
                        Phase::None
                    };
                }
                return;
            }
        }
        match self.phase {
            Phase::Starting => {
                if alive(sock) {
                    self.phase = Phase::Up;
                } else if self.started.elapsed() > Duration::from_secs(25) {
                    self.abort();
                    self.phase = Phase::Failed("prim room did not start in time".into());
                }
            }
            Phase::Up => {
                // A daemon we started is watched through its `Child` above. One we adopted
                // has no handle, so look at its socket now and then (every connect makes the
                // daemon set up, and drop, a local client; not something to do 4 times a second).
                if self.child.is_none() && self.probed.elapsed() > Duration::from_secs(5) {
                    self.probed = Instant::now();
                    if !alive(sock) {
                        self.phase = Phase::None;
                    }
                }
            }
            _ => {}
        }
    }

    pub fn abort(&mut self) {
        if let Some(mut c) = self.child.take() {
            procs::terminate(c.id() as i32);
            std::thread::spawn(move || {
                let _ = c.wait();
            });
        }
        self.phase = Phase::None;
    }

    /// Ask the daemon on `sock` to stop (the `Shutdown` request), waiting for the socket to
    /// go. Falls back to SIGTERM on a child we started.
    pub fn stop(&mut self, sock: &Path) {
        if alive(sock) {
            let c = Client::start(UnixTransport::new(sock));
            let deadline = Instant::now() + Duration::from_secs(2);
            while !c.is_connected() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = c.call(|req| Request::Shutdown { req }, Duration::from_secs(3));
            drop(c);
            let deadline = Instant::now() + Duration::from_secs(5);
            while alive(sock) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        if alive(sock) {
            if let Some(c) = &self.child {
                procs::terminate(c.id() as i32);
                let deadline = Instant::now() + Duration::from_secs(3);
                while alive(sock) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
        if let Some(mut c) = self.child.take() {
            std::thread::spawn(move || {
                let _ = c.wait();
            });
        }
        self.phase = Phase::None;
    }

    /// Tell the room we are leaving (so others see it), then stop the daemon.
    pub fn leave_and_stop(&mut self, sock: &Path) {
        if alive(sock) {
            let c = Client::start(UnixTransport::new(sock));
            let deadline = Instant::now() + Duration::from_secs(2);
            while !c.is_connected() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if let Ok(ReplyJson::Ok) = c.call(|req| Request::Leave { req }, Duration::from_secs(3)) {
                // Give the goodbye a moment to go out before the daemon stops.
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        self.stop(sock);
    }
}
