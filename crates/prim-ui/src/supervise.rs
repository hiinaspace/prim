//! Starting and watching local programs: the room daemon, `prim voice`, and the launch
//! table of other clients. Unix only, like the daemon itself.
//!
//! Everything we spawn gets its own process group and is not killed when the window
//! closes: closing the window keeps you in the room.

use std::collections::VecDeque;
use std::fs::File;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

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
    std::env::var_os("PRIM_SOCKET")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(prim_room::local::default_socket_path)
}

pub fn rt_socket_path(socket: &Path) -> PathBuf {
    socket.with_file_name("rt.sock")
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

fn command(args: &[String], log: &str) -> Result<Command> {
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

/// Start `prim room ...` for `mode` and wait until its socket answers. Extra daemon
/// arguments (relay, bind) come from `PRIM_UI_DAEMON_ARGS`, split on spaces.
pub fn start_daemon(mode: &StartMode, socket: &Path) -> Result<()> {
    if daemon_alive(socket) {
        bail!("a daemon is already running on {}", socket.display());
    }
    let mut args: Vec<String> = vec!["room".into()];
    match mode {
        StartMode::Found { meta } => {
            args.push("found".into());
            if !meta.trim().is_empty() {
                args.push(format!("--meta={}", meta.trim()));
            }
        }
        StartMode::Join(t) => args.extend(["join".into(), t.trim().to_string()]),
        StartMode::Resume(h) => args.extend(["resume".into(), h.clone()]),
    }
    args.push(format!("--socket={}", socket.display()));
    args.push(format!("--rt-socket={}", rt_socket_path(socket).display()));
    if let Ok(extra) = std::env::var("PRIM_UI_DAEMON_ARGS") {
        args.extend(extra.split_whitespace().map(str::to_string));
    }
    let mut child = command(&args, "daemon.log")?.spawn().context("spawn prim room")?;
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        if daemon_alive(socket) {
            return Ok(());
        }
        if let Some(st) = child.try_wait()? {
            bail!("prim room exited ({st}): {}", last_log_line("daemon.log"));
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            bail!("prim room did not start in time: {}", last_log_line("daemon.log"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Process ids of running `prim <sub>` programs (same binary name as this one) whose
/// `--<sock_flag>` value, or the default, equals `socket`.
pub fn find_procs(sub: &str, sock_flag: &str, socket: &Path) -> Vec<i32> {
    let me = std::process::id() as i32;
    let our_name = exe()
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut out = vec![];
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return out;
    };
    for e in rd.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else {
            continue;
        };
        if pid == me {
            continue;
        }
        let Ok(raw) = std::fs::read(e.path().join("cmdline")) else {
            continue;
        };
        let argv: Vec<String> = raw
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        let name = argv
            .first()
            .and_then(|a| Path::new(a).file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name != our_name || argv.get(1).map(String::as_str) != Some(sub) {
            continue;
        }
        let flag = format!("--{sock_flag}");
        let given = argv.iter().enumerate().find_map(|(i, a)| {
            if let Some(v) = a.strip_prefix(&format!("{flag}=")) {
                Some(v.to_string())
            } else if *a == flag {
                argv.get(i + 1).cloned()
            } else {
                None
            }
        });
        let matches = match given {
            Some(v) => Path::new(&v) == socket || Path::new(&v) == rt_socket_path(socket),
            None => sock_flag == "socket" && socket == prim_room::local::default_socket_path(),
        };
        if matches {
            out.push(pid);
        }
    }
    out
}

pub fn terminate(pid: i32) {
    // SAFETY: plain signal to a pid we found ourselves.
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
}

/// Stop the daemon on `socket` (found by scanning /proc; the local API has no shutdown).
pub fn stop_daemon(socket: &Path) -> bool {
    let pids = find_procs("room", "socket", socket);
    for p in &pids {
        terminate(*p);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while daemon_alive(socket) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
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

/// `prim voice` with the window's mic and deafen choices.
pub struct Voice {
    pub sup: Supervised,
    pub mic: bool,
    pub deaf: bool,
    socket: PathBuf,
    /// Private-sink mode for tests and screenshots (`PRIM_UI_VOICE_TEST=1`): a tone
    /// "microphone" and a null output, never real devices.
    test: bool,
}

impl Voice {
    pub fn new(socket: &Path) -> Self {
        Self {
            sup: Supervised::new("voice"),
            // The mic starts muted: being in the room without talking is the default.
            mic: false,
            deaf: false,
            socket: socket.to_path_buf(),
            test: std::env::var_os("PRIM_UI_VOICE_TEST").is_some(),
        }
    }

    fn args(&self, takeover: bool) -> Vec<String> {
        let mut a = vec![
            "voice".to_string(),
            format!("--rt-socket={}", rt_socket_path(&self.socket).display()),
        ];
        let capture = match (self.mic, self.test) {
            (false, _) => "none",
            (true, true) => "tone:auto",
            (true, false) => "device",
        };
        let output = if self.deaf || self.test { "null" } else { "device" };
        a.push(format!("--capture={capture}"));
        a.push(format!("--output={output}"));
        if takeover {
            a.push("--takeover".into());
        }
        a
    }

    pub fn start(&mut self, takeover: bool) {
        let args = self.args(takeover);
        if let Err(e) = self.sup.start(&args) {
            self.sup.state = ProcState::Failed(e.to_string());
        }
    }

    pub fn stop(&mut self) {
        self.sup.stop();
        // A voice client from an earlier window.
        for p in find_procs("voice", "rt-socket", &self.socket) {
            terminate(p);
        }
    }

    /// Mic or deafen changed: voice has no runtime control, so replace the client.
    /// The new one takes the lease over; the old one is told so by the daemon.
    pub fn restart_with(&mut self, mic: bool, deaf: bool) {
        self.mic = mic;
        self.deaf = deaf;
        for p in find_procs("voice", "rt-socket", &self.socket) {
            terminate(p);
        }
        self.sup.stop();
        self.start(true);
    }

    pub fn tick(&mut self) {
        if self.sup.poll(true) {
            self.start(true);
        }
    }
}
