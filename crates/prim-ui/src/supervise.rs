//! What the dev window still does by itself: launch other clients (`prim watch`) and short
//! audio helpers. The room daemon and `prim voice` are the session supervisor's
//! (`prim session`, crate `prim-session`); [`start_daemon`], [`stop_daemon`] and [`Voice`]
//! here only send it requests. Unix only, like the daemon itself.

use std::collections::VecDeque;
use prim_client::session::{SessionClient, SessionRequest};
use std::fs::File;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::Result;

/// The `prim` binary to spawn: this one, or `PRIM_UI_EXE` (tests run from another binary).
pub fn exe() -> PathBuf {
    std::env::var_os("PRIM_UI_EXE")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from("prim"))
}

/// Where the window keeps logs of what it spawned.
pub fn log_dir() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."));
    let d = base.join("prim").join("ui");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn socket_path() -> PathBuf {
    prim_client::paths::room_socket()
}

pub fn rt_socket_path(socket: &Path) -> PathBuf {
    socket.with_file_name("rt.sock")
}

/// Where `prim voice` publishes the live microphone level while it is running.
pub fn voice_levels_path(socket: &Path) -> PathBuf {
    socket.with_file_name("voice.levels")
}

/// Where the settings panel's own meter (`prim voice meter`) publishes it when voice is off.
pub fn meter_levels_path(socket: &Path) -> PathBuf {
    socket.with_file_name("meter.levels")
}

pub fn daemon_alive(socket: &Path) -> bool {
    UnixStream::connect(socket).is_ok()
}

fn log_file(name: &str) -> Result<File> {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir().join(name))?;
    use std::io::Write;
    let _ = writeln!(f, "--- {:?} ---", std::time::SystemTime::now());
    Ok(f)
}

fn last_log_line(name: &str) -> String {
    std::fs::read_to_string(log_dir().join(name))
        .ok()
        .and_then(|s| {
            s.lines()
                .rev()
                .find(|l| !l.trim().is_empty() && !l.starts_with("---"))
                .map(str::to_string)
        })
        .unwrap_or_default()
}

pub(crate) fn command(args: &[String], log: &str) -> Result<Command> {
    let mut c = Command::new(exe());
    c.args(args)
        .stdin(Stdio::null())
        .stdout(log_file(log)?)
        .stderr(log_file(log)?)
        .process_group(0);
    Ok(c)
}

#[derive(Clone, Debug)]
pub enum StartMode {
    Found { meta: String },
    Join(String),
    Resume(String),
}

/// Ask the session (starting it if none runs) to start the room daemon for `mode`.
pub fn start_daemon(mode: &StartMode, socket: &Path) -> Result<()> {
    let sc = SessionClient::new(socket);
    sc.ensure_running(socket)?;
    let req = match mode {
        StartMode::Found { meta } => SessionRequest::Create { name: meta.trim().to_string(), args: vec![] },
        StartMode::Join(t) => SessionRequest::Join { token: t.trim().to_string(), args: vec![] },
        StartMode::Resume(h) => SessionRequest::Resume { room: h.clone(), args: vec![] },
    };
    sc.call(&req).map(|_| ()).map_err(|e| anyhow::anyhow!("{e}"))
}

pub fn terminate(pid: i32) {
    // SAFETY: plain signal to a pid we started ourselves.
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
}

/// Leave the room and stop the daemon (and voice), through the session.
pub fn stop_daemon(socket: &Path) -> bool {
    let sc = SessionClient::new(socket);
    if sc.alive() {
        let _ = sc.call(&SessionRequest::Leave);
    }
    !daemon_alive(socket)
}

/// Rooms with saved state on this machine, newest first: `(room hex, modified)`.
pub fn saved_rooms() -> Vec<(String, std::time::SystemTime)> {
    let mut v: Vec<_> = std::fs::read_dir(prim_room::state::rooms_root())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let md = e.path().join("room.bin").metadata().ok()?;
            let t = md.modified().ok()?;
            (name.len() == 64 && !name.starts_with('.')).then_some((name, t))
        })
        .collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v
}

// ---- supervised programs --------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum ProcState {
    Off,
    Running,
    /// Exited and will be relaunched.
    Restarting(String),
    /// `prim voice` exit 3: another program holds the lease.
    Busy,
    Failed(String),
}

/// One program the window started and watches.
pub struct Supervised {
    pub id: &'static str,
    pub state: ProcState,
    child: Option<Child>,
    restarts: VecDeque<Instant>,
}

impl Supervised {
    pub fn new(id: &'static str) -> Self {
        Self {
            id,
            state: ProcState::Off,
            child: None,
            restarts: VecDeque::new(),
        }
    }

    pub fn running(&self) -> bool {
        self.child.is_some()
    }

    pub fn start(&mut self, args: &[String]) -> Result<()> {
        self.stop(); // never overwrite (and so orphan) a live child
        let c = command(args, &format!("{}.log", self.id))?.spawn()?;
        self.child = Some(c);
        self.state = ProcState::Running;
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(mut c) = self.child.take() {
            terminate(c.id() as i32);
            // Reap off-thread so the window never waits on it.
            std::thread::spawn(move || {
                let _ = c.wait();
            });
        }
        self.state = ProcState::Off;
    }

    /// Notice an exit. Returns true when the program should be started again now
    /// (`relaunch`: only for exits that are expected, with a rate limit).
    pub fn poll(&mut self, relaunch: bool) -> bool {
        let Some(c) = &mut self.child else {
            return false;
        };
        let Ok(Some(st)) = c.try_wait() else {
            return false;
        };
        self.child = None;
        let code = st.code();
        let log = format!("{}.log", self.id);
        match code {
            Some(0) | None => self.state = ProcState::Off,
            Some(3) => self.state = ProcState::Busy,
            Some(5) => {
                self.state = ProcState::Failed("the room daemon went away".into());
            }
            Some(4) if relaunch => {
                let now = Instant::now();
                self.restarts.retain(|t| now.duration_since(*t) < Duration::from_secs(30));
                if self.restarts.len() >= 5 {
                    self.state = ProcState::Failed("keeps stopping; not restarting".into());
                } else {
                    self.restarts.push_back(now);
                    self.state = ProcState::Restarting("lost its lease; restarting".into());
                    return true;
                }
            }
            Some(n) => {
                let tail = last_log_line(&log);
                self.state = ProcState::Failed(format!("exited with {n}: {tail}"));
            }
        }
        false
    }
}

/// The session's voice client as the window sees it: it asks, the session does.
pub struct Voice {
    pub sup: VoiceView,
    pub mic: bool,
    pub deaf: bool,
    socket: PathBuf,
    test: bool,
    last: Instant,
}

/// Running state of the session's voice client.
pub struct VoiceView {
    pub state: ProcState,
    running: bool,
}

impl VoiceView {
    pub fn running(&self) -> bool {
        self.running
    }
}

impl Voice {
    pub fn new(socket: &Path) -> Self {
        Self {
            sup: VoiceView { state: ProcState::Off, running: false },
            mic: false,
            deaf: false,
            socket: socket.to_path_buf(),
            test: std::env::var_os("PRIM_UI_VOICE_TEST").is_some()
                || std::env::var_os("PRIM_SESSION_VOICE_TEST").is_some(),
            last: Instant::now() - Duration::from_secs(10),
        }
    }

    /// Tone microphone and null speakers instead of real devices.
    pub fn test_mode(&self) -> bool {
        self.test
    }

    fn ask(&mut self, req: SessionRequest) {
        let sc = SessionClient::new(&self.socket);
        match sc.ensure_running(&self.socket).and_then(|_| sc.call(&req)) {
            Ok(st) => self.apply(&st),
            Err(e) => self.sup.state = ProcState::Failed(e.to_string()),
        }
    }

    fn apply(&mut self, st: &prim_client::session::SessionStatus) {
        let v = &st.voice;
        self.mic = v.mic;
        self.deaf = v.deaf;
        self.sup.running = v.running;
        self.sup.state = match v.state.as_str() {
            "running" => ProcState::Running,
            "restarting" => ProcState::Restarting(v.detail.clone()),
            "busy" => ProcState::Busy,
            "failed" => ProcState::Failed(v.detail.clone()),
            _ => ProcState::Off,
        };
    }

    /// Turn voice on (muted, hearing). `takeover`: the session replaces another program's.
    pub fn start(&mut self, _takeover: bool) {
        self.ask(SessionRequest::Voice { on: true });
    }

    pub fn stop(&mut self) {
        self.ask(SessionRequest::Voice { on: false });
    }

    pub fn restart_with(&mut self, mic: bool, deaf: bool) {
        self.ask(SessionRequest::SetMic { on: mic });
        self.ask(SessionRequest::SetDeafen { on: deaf });
    }

    /// Refresh from the session about twice a second.
    pub fn tick(&mut self) {
        if self.last.elapsed() < Duration::from_millis(500) {
            return;
        }
        self.last = Instant::now();
        if let Ok(st) = SessionClient::new(&self.socket).status() {
            self.apply(&st);
        }
    }
}
