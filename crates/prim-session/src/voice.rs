//! `prim voice` as a child of the session: kept running while a room is up, relaunched
//! when it loses its lease, replaced (for now) when mic or deafen change.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{Duration, Instant};

use prim_client::paths;
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
    restarts: VecDeque<Instant>,
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
            restarts: VecDeque::new(),
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

    fn args(&self, sock: &Path, takeover: bool) -> Vec<String> {
        let capture = match (self.mic, self.test) {
            (false, _) => "none",
            (true, true) => "tone:auto",
            (true, false) => "device",
        };
        let output = if self.deaf || self.test { "null" } else { "device" };
        let mut a = vec![
            "voice".to_string(),
            format!("--rt-socket={}", paths::rt_socket(sock).display()),
            format!("--levels={}", paths::voice_levels(sock).display()),
            format!("--capture={capture}"),
            format!("--output={output}"),
        ];
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

    /// Round VC replaces the body of this with a request to the running client.
    pub fn set_state(&mut self, mic: bool, deaf: bool, room_up: bool, sock: &Path) {
        let changed = (mic, deaf) != (self.mic, self.deaf);
        self.mic = mic;
        self.deaf = deaf;
        if !self.running() {
            if self.wanted && room_up {
                self.start(sock, true);
            }
            return;
        }
        if changed {
            self.kill_running(sock);
            self.child = None;
            self.start(sock, true);
        }
    }

    /// Find a voice client for this room that is already running and treat it as ours.
    /// Its mic and deafen state is read from its own command line.
    pub fn adopt(&mut self, sock: &Path) {
        let found = procs::find("voice", "rt-socket", &[paths::rt_socket(sock)], false);
        if let Some((pid, argv)) = found.into_iter().next() {
            self.mic = procs::flag_value(&argv, "capture").is_some_and(|c| c != "none");
            self.deaf = procs::flag_value(&argv, "output").is_some_and(|o| o == "null") && !self.test;
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
                        let now = Instant::now();
                        self.restarts.retain(|t| now.duration_since(*t) < Duration::from_secs(30));
                        if self.restarts.len() >= 5 {
                            self.state = "failed";
                            self.detail = "keeps stopping; not restarting".into();
                        } else {
                            self.restarts.push_back(now);
                            self.state = "restarting";
                            self.detail = "lost its lease; restarting".into();
                            self.start(sock, true);
                        }
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
        // Voice follows the room: start it when a room is up, drop it when the room is gone.
        if self.wanted && room_up && !self.running() && self.state == "off" {
            self.start(sock, false);
        }
        if !room_up && self.running() && self.child.is_some() {
            // Voice exits by itself (code 5) when the daemon is gone; nothing to do.
        }
    }
}
