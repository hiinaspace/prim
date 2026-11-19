//! `prim voice` as a child of the session: kept running while a room is up. Mic and
//! deafen are requests to the running client (round VC: `Client::voice_control`); the
//! process is never restarted for them, so the stream and its incarnation are unchanged.
//! A lost lease is retried inside the voice process, so the session only starts voice when
//! none is running (`VoiceError::NotRunning`). Exit 4 now means another program took the
//! lease over (or policy ended it): the session does not fight back, the person decides.
//!
//! Fail-safe rules (a live microphone with nothing visible is the one thing never to do):
//! - every start the session makes is `--start-muted`; the mic goes live only through an
//!   explicit request (`voice_control`) from an attached client;
//! - `mic`/`deaf` in the view are what voice last reported, never what was asked for;
//! - a request that cannot be applied yet (voice starting, reconnecting, no ack) stays
//!   `pending` and is retried by the supervisor until voice acknowledges it;
//! - no IPC with the voice client happens while the session state lock is held: the
//!   supervisor takes a [`Plan`] under the lock, talks unlocked, and applies the outcome.

use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{Duration, Instant};

use prim_client::client::{Client, VoiceError};
use prim_client::paths;
use prim_client::transport::UnixTransport;
use prim_client::wire::VoiceStatus;
use prim_client::session::VoiceState;

use crate::procs;

/// A change of mic and/or deafen that was asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Req {
    pub mic: Option<bool>,
    pub deaf: Option<bool>,
}

impl Req {
    fn merge(self, newer: Req) -> Req {
        Req { mic: newer.mic.or(self.mic), deaf: newer.deaf.or(self.deaf) }
    }
}

/// What the supervisor does with the voice client without holding the state lock.
pub struct Plan {
    pending: Option<Req>,
}

/// The outcome of a [`Plan`].
pub struct Probe {
    status: Result<VoiceStatus, VoiceError>,
    applied: Option<(Req, Result<VoiceStatus, VoiceError>)>,
}

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
    /// A mic/deafen request that voice has not acknowledged yet (still starting, reconnecting).
    pending: Option<Req>,
    pending_since: Option<Instant>,
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
            pending: None,
            pending_since: None,
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

    /// Voice starts once, muted, with the wanted deafen state; later changes are requests.
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
        // Always muted: only an explicit request from an attached client opens the mic.
        a.push("--start-muted".into());
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
                // The new process is muted whatever the last one was doing; an old unapplied
                // request must not unmute a relaunch.
                self.mic = false;
                self.pending = None;
                self.pending_since = None;
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
        self.pending = None;
        self.pending_since = None;
        // Next time voice is turned on it starts muted and hearing, as documented.
        self.mic = false;
        self.deaf = false;
    }

    /// Someone turned voice on. If another program held the lease (or voice kept
    /// failing), this is the person saying "take it": start with `--takeover`.
    pub fn turn_on(&mut self, room_up: bool, sock: &Path) {
        self.wanted = true;
        if !self.running() && matches!(self.state, "busy" | "failed") {
            self.state = "off";
            self.detail.clear();
            if room_up {
                self.start(sock, true);
            }
        }
    }

    /// A request for mic/deafen. With no voice process, start one (muted) and keep the request
    /// pending until it has a lease. With a process (leased or not) return the request for the
    /// caller to send unlocked: a live process is never replaced for a toggle.
    pub fn request(&mut self, req: Req, room_up: bool, sock: &Path) -> Option<Req> {
        if self.running() {
            return Some(req);
        }
        if self.wanted && room_up {
            self.start(sock, true);
            self.set_pending(req);
        }
        None
    }

    fn set_pending(&mut self, req: Req) {
        self.pending = Some(self.pending.map_or(req, |p| p.merge(req)));
        self.pending_since.get_or_insert_with(Instant::now);
    }

    /// What voice reports is the truth (it may have muted itself); requests don't edit it.
    pub fn learn(&mut self, st: &VoiceStatus) {
        if st.voice_running {
            self.mic = st.mic;
            self.deaf = st.deaf;
        }
    }

    /// Fold in the outcome of a request sent to the voice client.
    pub fn applied(&mut self, req: Req, r: Result<VoiceStatus, VoiceError>) {
        match r {
            Ok(st) => {
                self.learn(&st);
                self.pending = None;
                self.pending_since = None;
                self.detail.clear();
            }
            Err(e) => {
                // Not acknowledged (starting, reconnecting, no ack): keep it and retry.
                self.set_pending(req);
                self.detail = format!("mic/deafen change not applied yet ({e}); retrying");
            }
        }
    }

    /// Under the lock: should the supervisor talk to the voice client now?
    pub fn plan(&mut self, room_up: bool) -> Option<Plan> {
        let every = if self.pending.is_some() { Duration::from_millis(250) } else { Duration::from_secs(1) };
        if !(self.running() && room_up && self.last_probe.is_none_or(|t| t.elapsed() >= every)) {
            return None;
        }
        self.last_probe = Some(Instant::now());
        Some(Plan { pending: self.pending })
    }

    /// Under the lock again: apply what the unlocked probe found.
    pub fn finish_probe(&mut self, p: Probe) {
        if let Ok(st) = &p.status {
            self.learn(st);
        }
        if let Some((req, r)) = p.applied {
            self.applied(req, r);
        }
        if self.pending.is_some() && self.pending_since.is_some_and(|t| t.elapsed() > Duration::from_secs(10)) {
            self.detail = "voice process is up but holds no lease; mic/deafen change is waiting".into();
        }
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
                    self.mic = false; // unknown: report muted until its own report arrives
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
/// never a restart (a process that is really gone is started again by `poll`).
pub fn control(sock: &Path, req: Req, tries: u32) -> Result<VoiceStatus, VoiceError> {
    let tray = crate::tray::live();
    let c = connect(sock).ok_or_else(|| VoiceError::Other("room daemon not reachable".into()))?;
    for _ in 0..tries.max(1) {
        match c.voice_control(req.mic, req.deaf, Some(tray)) {
            Err(VoiceError::Reconnecting) => std::thread::sleep(Duration::from_millis(300)),
            r => return r,
        }
    }
    Err(VoiceError::Reconnecting)
}

/// Unlocked: read what voice does and, when a request is pending and voice is up, send it
/// (one try; the supervisor comes back in 250 ms).
pub fn probe(sock: &Path, plan: Plan) -> Probe {
    let status = status(sock);
    let applied = match (&status, plan.pending) {
        (Ok(st), Some(req)) if st.voice_running => Some((req, control(sock, req, 1))),
        _ => None,
    };
    Probe { status, applied }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(mic: bool, deaf: bool) -> VoiceStatus {
        VoiceStatus { voice_running: true, mic, deaf, ..Default::default() }
    }

    #[test]
    fn a_running_process_is_never_replaced_for_a_toggle() {
        let mut v = Voice::new(true);
        v.adopted = Some(std::process::id() as i32); // any live pid: "a voice that is up"
        let req = Req { mic: Some(true), deaf: None };
        assert_eq!(v.request(req, true, Path::new("/nonexistent/room.sock")), Some(req));
        assert_eq!(v.starts, 0, "no start was made");
    }

    #[test]
    fn an_unacknowledged_request_stays_pending_and_status_does_not_run_ahead() {
        let mut v = Voice::new(true);
        let mute = Req { mic: Some(false), deaf: None };
        v.mic = true; // voice reports a live mic
        v.applied(mute, Err(VoiceError::NoAck));
        assert!(v.mic, "status still shows what voice reported, not what was asked");
        assert_eq!(v.pending, Some(mute), "the mute is kept for retry");
        assert!(v.detail.contains("not applied"), "{}", v.detail);
        // A later probe's status read must not erase the pending mute.
        v.learn(&st(true, false));
        assert_eq!(v.pending, Some(mute));
        // Acknowledged: now it clears and the report is adopted.
        v.applied(mute, Ok(st(false, false)));
        assert!(!v.mic && v.pending.is_none() && v.detail.is_empty());
    }

    #[test]
    fn pending_requests_merge_newest_wins() {
        let mut v = Voice::new(true);
        v.applied(Req { mic: Some(true), deaf: Some(true) }, Err(VoiceError::Reconnecting));
        v.applied(Req { mic: Some(false), deaf: None }, Err(VoiceError::Reconnecting));
        assert_eq!(v.pending, Some(Req { mic: Some(false), deaf: Some(true) }));
    }

    #[test]
    fn start_args_are_always_muted() {
        let mut v = Voice::new(true);
        v.mic = true; // a stale "live" belief must not reach a relaunch
        v.deaf = true;
        let a = v.args(Path::new("/x/room.sock"), false);
        assert!(a.iter().any(|x| x == "--start-muted"), "{a:?}");
        assert!(a.iter().any(|x| x == "--start-deaf"), "{a:?}");
    }
}
