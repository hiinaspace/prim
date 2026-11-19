//! `prim voice` as a child of the session: kept running while a room is up. Mic and
//! deafen are requests to the running client (round VC: `Client::voice_control`); the
//! process is never restarted for them, so the stream and its incarnation are unchanged.
//! A lost lease is retried inside the voice process, so the session only starts voice when
//! none is running (`VoiceError::NotRunning`). Exit 4 now means another program took the
//! lease over (or policy ended it): the session does not fight back, the person decides.

use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{Duration, Instant};

use prim_client::client::{Client, VoiceError};
use prim_client::paths;
use prim_client::transport::UnixTransport;
use prim_client::wire::VoiceStatus;
use prim_client::session::VoiceState;

use crate::procs;

pub struct Voice {
    /// The session starts voice by itself once a room is up.
    pub wanted: bool,
    pub mic: bool,
    pub deaf: bool,
    child: Option<Child>,
    /// A voice client left by an earlier session (we have no `Child`, only its pid).
    adopted: Option<i32>,
    state: &'static str,
    detail: String,
    starts: u32,
    /// A mic/deafen change arrived while voice was still starting (no lease yet): apply it
    /// as soon as the client reports.
    pending: bool,
    started: Option<Instant>,
    last_probe: Option<Instant>,
    /// Private-sink mode for tests: a tone "microphone" and a null output, never real devices.
    test: bool,
}

impl Voice {
    pub fn new(auto: bool) -> Voice {
        Voice {
            wanted: auto,
            // The mic starts muted: being in the room without talking is the default.
            mic: false,
            deaf: false,
            child: None,
            adopted: None,
            state: "off",
            detail: String::new(),
            starts: 0,
            pending: false,
            started: None,
            last_probe: None,
            test: std::env::var_os("PRIM_SESSION_VOICE_TEST").is_some()
                || std::env::var_os("PRIM_UI_VOICE_TEST").is_some(),
        }
    }

    fn pid(&self) -> Option<i32> {
        self.child.as_ref().map(|c| c.id() as i32).or(self.adopted)
    }

    pub fn running(&self) -> bool {
        self.pid().is_some()
    }

    pub fn view(&self) -> VoiceState {
        VoiceState {
            running: self.running(),
            wanted: self.wanted,
            mic: self.mic,
            deaf: self.deaf,
            state: self.state.to_string(),
            detail: self.detail.clone(),
            pid: self.pid().map(|p| p as u32),
            starts: self.starts,
        }
    }

    /// Voice starts once, with the wanted state in its flags; later changes are requests.
    fn args(&self, sock: &Path, takeover: bool) -> Vec<String> {
        let capture = if self.test { "tone:auto" } else { "device" };
        let output = if self.test { "null" } else { "device" };
        let mut a = vec![
            "voice".to_string(),
            format!("--rt-socket={}", paths::rt_socket(sock).display()),
            format!("--levels={}", paths::voice_levels(sock).display()),
            format!("--capture={capture}"),
            format!("--output={output}"),
        ];
        if !self.mic {
            a.push("--start-muted".into());
        }
        if self.deaf {
            a.push("--start-deaf".into());
        }
        if crate::tray::live() {
            a.push("--tray-live".into());
        }
        if takeover {
            a.push("--takeover".into());
        }
        a
    }

    /// Keep this a single place: how a start is made.
    fn start(&mut self, sock: &Path, takeover: bool) {
        let args = self.args(sock, takeover);
        match procs::spawn(&args, "voice.log") {
            Ok(c) => {
                self.child = Some(c);
                self.adopted = None;
                self.state = "running";
                self.detail.clear();
                self.starts += 1;
                self.started = Some(Instant::now());
                self.pending = false;
            }
            Err(e) => {
                self.state = "failed";
                self.detail = e.to_string();
            }
        }
    }

    fn kill_running(&mut self, sock: &Path) {
        if let Some(mut c) = self.child.take() {
            procs::terminate(c.id() as i32);
            std::thread::spawn(move || {
                let _ = c.wait();
            });
        }
        if let Some(p) = self.adopted.take() {
            procs::terminate(p);
        }
        // Any other voice client on this room (a stray from an older window).
        for (pid, _) in procs::find("voice", "rt-socket", &[paths::rt_socket(sock)], false) {
            procs::terminate(pid);
        }
    }

    pub fn stop(&mut self) {
        let sock = std::env::var_os("PRIM_SOCKET")
            .map(PathBuf::from)
            .unwrap_or_else(paths::room_socket);
        self.stop_at(&sock);
    }

    pub fn stop_at(&mut self, sock: &Path) {
        self.kill_running(sock);
        self.state = "off";
        self.detail.clear();
        // Next time voice is turned on it starts muted and hearing, as documented.
        self.mic = false;
        self.deaf = false;
    }

    /// Someone turned voice on. If another program held the lease (or voice kept
    /// failing), this is the person saying "take it": start with `--takeover`.
    pub fn turn_on(&mut self, room_up: bool, sock: &Path) {
        self.wanted = true;
        if !self.running() && matches!(self.state, "busy" | "failed") {
            self.restarts.clear();
            self.state = "off";
            self.detail.clear();
            if room_up {
                self.start(sock, true);
            }
        }
    }

    /// Record the wanted mic/deafen. With no voice client running, start one if wanted.
    /// Returns true when a running client must be told (`control`, outside the lock).
    pub fn want(&mut self, mic: bool, deaf: bool, room_up: bool, sock: &Path) -> bool {
        self.mic = mic;
        self.deaf = deaf;
        if !self.running() {
            if self.wanted && room_up {
                self.start(sock, true);
            }
            return false;
        }
        true
    }

    /// Adopt what the running client reports as the truth (it may have muted itself).
    pub fn learn(&mut self, st: &VoiceStatus) {
        if st.voice_running {
            self.mic = st.mic;
            self.deaf = st.deaf;
            self.pending = false;
        }
    }

    /// A process we started is still within its first seconds: it has no lease yet.
    pub fn running_unleased(&self) -> bool {
        self.running() && self.started.is_some_and(|t| t.elapsed() < Duration::from_secs(5))
    }

    /// `NotRunning` for a client that should exist: start it with the wanted state.
    pub fn restart_missing(&mut self, mic: bool, deaf: bool, room_up: bool, sock: &Path) {
        self.child = None;
        self.adopted = None;
        self.mic = mic;
        self.deaf = deaf;
        if self.wanted && room_up {
            self.start(sock, true);
        }
    }

    pub fn note_not_running(&mut self) {
        // Still starting: apply the wanted state when the client reports.
        self.pending = true;
    }

    /// Find a voice client for this room that is already running and treat it as ours.
    /// Its mic and deafen state is read from its own command line.
    pub fn adopt(&mut self, sock: &Path) {
        let found = procs::find("voice", "rt-socket", &[paths::rt_socket(sock)], false);
        if let Some((pid, argv)) = found.into_iter().next() {
            // Mic and deafen come from the client's own report; the command line only
            // says how it started, which is stale once it has been muted at run time.
            match status(sock) {
                Ok(st) if st.voice_running => self.learn(&st),
                _ => {
                    self.mic = !argv.iter().any(|a| a == "--start-muted");
                    self.deaf = argv.iter().any(|a| a == "--start-deaf");
                }
            }
            self.adopted = Some(pid);
            self.state = "running";
            self.wanted = true;
        }
    }

    pub fn poll(&mut self, room_up: bool, sock: &Path) {
        if let Some(c) = &mut self.child {
            if let Ok(Some(st)) = c.try_wait() {
                self.child = None;
                match st.code() {
                    Some(0) | None => self.state = "off",
                    Some(3) => {
                        self.state = "busy";
                        self.detail = "another program holds the voice lease".into();
                    }
                    Some(5) => {
                        self.state = "off";
                        self.detail = "the room daemon went away".into();
                    }
                    Some(4) => {
                        // Takeover or policy only (a lost lease is retried in-process).
                        self.state = "busy";
                        self.detail = "another voice client took over, or policy ended it".into();
                    }
                    Some(n) => {
                        self.state = "failed";
                        self.detail = format!("exited with {n}: {}", procs::last_log_line("voice.log"));
                    }
                }
            }
        }
        if let Some(p) = self.adopted {
            if !procs::pid_alive(p) {
                self.adopted = None;
                self.state = "off";
            }
        }
        // Learn what the client really does (it can mute itself), about once a second.
        if self.running() && room_up && self.last_probe.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
            self.last_probe = Some(Instant::now());
            if let Ok(st) = status(sock) {
                if self.pending && st.voice_running {
                    // A change arrived during startup: apply it now.
                    let _ = control(sock, Some(self.mic), Some(self.deaf));
                    self.pending = false;
                } else {
                    self.learn(&st);
                }
            }
        }
        // Voice follows the room: start it when a room is up, drop it when the room is gone.
        if self.wanted && room_up && !self.running() && self.state == "off" {
            self.start(sock, false);
        }
        if !room_up && self.running() && self.child.is_some() {
            // Voice exits by itself (code 5) when the daemon is gone; nothing to do.
        }
    }
}

fn connect(sock: &Path) -> Option<Client> {
    let c = Client::start(UnixTransport::new(sock));
    let end = Instant::now() + Duration::from_millis(500);
    for _ in 0..50 {
        if c.is_connected() || Instant::now() >= end {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    c.is_connected().then_some(c)
}

fn status(sock: &Path) -> Result<VoiceStatus, VoiceError> {
    match connect(sock) {
        Some(c) => c.voice_status(),
        None => Err(VoiceError::Other("room daemon not reachable".into())),
    }
}

/// Ask the running voice client to change mic/deafen. `voice_reconnecting` is wait-and-retry,
/// never a restart; the caller restarts only on `NotRunning`.
pub fn control(sock: &Path, mic: Option<bool>, deaf: Option<bool>) -> Result<VoiceStatus, VoiceError> {
    let tray = crate::tray::live();
    let c = connect(sock).ok_or_else(|| VoiceError::Other("room daemon not reachable".into()))?;
    for _ in 0..12 {
        match c.voice_control(mic, deaf, Some(tray)) {
            Err(VoiceError::Reconnecting) => std::thread::sleep(Duration::from_millis(300)),
            r => return r,
        }
    }
    Err(VoiceError::Reconnecting)
}
