//! `prim voice` as a child of the session: kept running while a room is up.
//!
//! Mic and deafen are **level-triggered**: a request only writes `desired` (with a
//! monotonic `gen`); one thread, the supervisor, owns every conversation with the voice
//! client and always sends the *full* desired state. Nothing is replayed from an older
//! request, so a late mute cannot be overtaken by an older unmute.
//!
//! - `desired` / `gen`: what the person asked for last. Requests never talk to voice.
//! - `mic` / `deaf` / `reported_gen`: what voice last acknowledged, and for which `gen`.
//!   `reported_gen < gen` is "applying" (the `pending` flag in the status).
//! - A reply counts only for the process it was sent to, and a status read counts only if
//!   no request or ack happened since it was taken.
//! - Every start the session makes is `--start-muted` and sets `desired.mic = false`;
//!   `--start-deaf` follows `desired.deaf`. Only an explicit request opens the mic.
//! - A toggle never starts, replaces or takes over a voice process; only `Voice{on}`
//!   (`turn_on`) and the room coming up (`poll`) start one.
//! - The process is never restarted for a toggle, so the stream and incarnation stay. Exit 4
//!   means another program took the lease over (or policy ended it): not fought.
//! - No IPC happens while the session state lock is held: the supervisor takes a [`Job`]
//!   under the lock, talks to voice unlocked, and hands the [`JobResult`] back to `finish`.

use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{Duration, Instant};

use prim_client::client::{Client, VoiceError};
use prim_client::paths;
use prim_client::session::VoiceState;
use prim_client::transport::UnixTransport;
use prim_client::wire::VoiceStatus;

use crate::procs;

/// What the person wants voice to do. Always sent whole.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Desired {
    pub mic: bool,
    pub deaf: bool,
}

/// The one way to talk to a running voice client (faked in tests).
pub trait Link {
    fn status(&self, sock: &Path) -> Result<VoiceStatus, VoiceError>;
    fn control(&self, sock: &Path, want: Desired) -> Result<VoiceStatus, VoiceError>;
}

/// The real thing: the room daemon's local API (`voice_control` / `voice_status`).
pub struct Daemon;

impl Link for Daemon {
    fn status(&self, sock: &Path) -> Result<VoiceStatus, VoiceError> {
        connect(sock)?.voice_status()
    }

    fn control(&self, sock: &Path, want: Desired) -> Result<VoiceStatus, VoiceError> {
        let tray = crate::tray::live();
        let c = connect(sock)?;
        // `voice_reconnecting` is wait-and-retry, never a restart. One short wait here; the
        // supervisor comes back for more.
        for _ in 0..3 {
            match c.voice_control(Some(want.mic), Some(want.deaf), Some(tray)) {
                Err(VoiceError::Reconnecting) => std::thread::sleep(Duration::from_millis(300)),
                r => return r,
            }
        }
        Err(VoiceError::Reconnecting)
    }
}

fn connect(sock: &Path) -> Result<Client, VoiceError> {
    let c = Client::start(UnixTransport::new(sock));
    for _ in 0..50 {
        if c.is_connected() {
            return Ok(c);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(VoiceError::Other("room daemon not reachable".into()))
}

/// One conversation with the voice client, planned under the lock.
pub struct Job {
    pid: i32,
    gen: u64,
    acks: u64,
    want: Desired,
    /// Send `want` (true) or only read the status (false).
    send: bool,
}

pub struct JobResult {
    job: Job,
    res: Result<VoiceStatus, VoiceError>,
}

/// Unlocked: do the conversation.
pub fn run_job(link: &dyn Link, sock: &Path, job: Job) -> JobResult {
    let res = if job.send { link.control(sock, job.want) } else { link.status(sock) };
    JobResult { job, res }
}

pub struct Voice {
    /// The session starts voice by itself once a room is up.
    pub wanted: bool,
    /// What voice last reported (never what was asked for).
    pub mic: bool,
    pub deaf: bool,
    desired: Desired,
    gen: u64,
    reported_gen: u64,
    /// Bumped on every acknowledgement and every start: a status read taken before is stale.
    acks: u64,
    child: Option<Child>,
    /// A voice client left by an earlier session (we have no `Child`, only its pid).
    adopted: Option<i32>,
    state: &'static str,
    detail: String,
    starts: u32,
    last_probe: Option<Instant>,
    retry_at: Option<Instant>,
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
            desired: Desired::default(),
            gen: 0,
            reported_gen: 0,
            acks: 0,
            child: None,
            adopted: None,
            state: "off",
            detail: String::new(),
            starts: 0,
            last_probe: None,
            retry_at: None,
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

    /// Voice has acknowledged everything asked so far (or there is nothing to apply it to).
    pub fn settled(&self) -> bool {
        !self.running() || self.reported_gen >= self.gen
    }

    pub fn gen(&self) -> u64 {
        self.gen
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
            pending: self.running() && self.reported_gen < self.gen,
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
            // Always muted: only an explicit request from an attached client opens the mic.
            "--start-muted".to_string(),
        ];
        if self.desired.deaf {
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
                self.started_as_desired();
            }
            Err(e) => {
                self.state = "failed";
                self.detail = e.to_string();
            }
        }
    }

    /// A new process starts muted and with `desired.deaf`: that is also what is desired now.
    fn started_as_desired(&mut self) {
        self.desired.mic = false;
        self.gen += 1;
        self.reported_gen = self.gen;
        self.mic = false;
        self.deaf = self.desired.deaf;
        self.acks += 1;
        self.retry_at = None;
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
        self.desired = Desired::default();
        self.gen += 1;
        self.reported_gen = self.gen;
        self.acks += 1;
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

    /// A request for mic and/or deafen: write `desired` and bump `gen`, nothing else. It never
    /// starts, replaces or takes over a voice process and never talks to voice. Returns the
    /// new `gen`; the supervisor applies it.
    pub fn set_desired(&mut self, mic: Option<bool>, deaf: Option<bool>) -> u64 {
        if let Some(m) = mic {
            self.desired.mic = m;
        }
        if let Some(d) = deaf {
            self.desired.deaf = d;
        }
        self.gen += 1;
        self.gen
    }

    /// Under the lock: what should the supervisor say to the voice client next, if anything?
    pub fn next_job(&mut self, room_up: bool) -> Option<Job> {
        let pid = self.pid()?;
        if !room_up {
            return None;
        }
        let now = Instant::now();
        if self.reported_gen < self.gen {
            if self.retry_at.is_some_and(|t| now < t) {
                return None;
            }
            return Some(Job { pid, gen: self.gen, acks: self.acks, want: self.desired, send: true });
        }
        if self.last_probe.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
            self.last_probe = Some(now);
            return Some(Job { pid, gen: self.gen, acks: self.acks, want: self.desired, send: false });
        }
        None
    }

    /// Under the lock again: fold in the outcome of a conversation.
    pub fn finish(&mut self, r: JobResult) {
        let JobResult { job, res } = r;
        // A reply for another process (voice was replaced meanwhile) says nothing about this one.
        if self.pid() != Some(job.pid) {
            return;
        }
        match (job.send, res) {
            (true, Ok(st)) => {
                // Voice applied the full state that was sent for `job.gen`. If a newer request
                // arrived meanwhile, `reported_gen < gen` and the next job sends that one.
                self.acks += 1;
                self.retry_at = None;
                self.detail.clear();
                if st.voice_running {
                    self.mic = st.mic;
                    self.deaf = st.deaf;
                }
                self.reported_gen = self.reported_gen.max(job.gen);
            }
            (true, Err(e)) => {
                // Changes nothing but the note and the retry timer.
                self.retry_at = Some(Instant::now() + Duration::from_millis(250));
                self.detail = format!("mic/deafen change not applied yet ({e}); retrying");
            }
            (false, Ok(st)) => {
                // Only a read taken with nothing in flight and nothing acked since is current.
                if st.voice_running && st.reported && job.gen == self.gen && job.acks == self.acks && self.reported_gen >= self.gen {
                    self.mic = st.mic;
                    self.deaf = st.deaf;
                    if st.mic != self.desired.mic {
                        if !st.mic {
                            // Voice muted itself (its safety rule): that is now what is wanted,
                            // or the session would unmute it again.
                            self.desired.mic = false;
                            self.gen += 1;
                            self.reported_gen = self.gen;
                        } else {
                            // A live mic that should be muted: send the desired state again.
                            self.gen += 1;
                        }
                    }
                    if st.deaf != self.desired.deaf {
                        self.gen += 1;
                    }
                }
            }
            (false, Err(_)) => {}
        }
    }

    /// Find a voice client for this room that is already running and treat it as ours.
    /// Its mic and deafen state is read from its own report.
    pub fn adopt(&mut self, link: &dyn Link, sock: &Path) {
        let found = procs::find("voice", "rt-socket", &[paths::rt_socket(sock)], false);
        if let Some((pid, argv)) = found.into_iter().next() {
            match link.status(sock) {
                Ok(st) if st.voice_running && st.reported => {
                    self.mic = st.mic;
                    self.deaf = st.deaf;
                }
                _ => {
                    // Unknown: report muted until its own report arrives.
                    self.mic = false;
                    self.deaf = argv.iter().any(|a| a == "--start-deaf");
                }
            }
            self.desired = Desired { mic: self.mic, deaf: self.deaf };
            self.gen += 1;
            self.reported_gen = self.gen;
            self.acks += 1;
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn st(mic: bool, deaf: bool) -> VoiceStatus {
        VoiceStatus { voice_running: true, reported: true, mic, deaf, ..Default::default() }
    }

    /// A voice that is "up": any live pid will do; no process is touched.
    fn up() -> Voice {
        let mut v = Voice::new(true);
        v.adopted = Some(std::process::id() as i32);
        v.state = "running";
        v
    }

    struct Fake {
        sent: RefCell<Vec<Desired>>,
        status: RefCell<VoiceStatus>,
    }

    impl Link for Fake {
        fn status(&self, _: &Path) -> Result<VoiceStatus, VoiceError> {
            Ok(self.status.borrow().clone())
        }
        fn control(&self, _: &Path, want: Desired) -> Result<VoiceStatus, VoiceError> {
            self.sent.borrow_mut().push(want);
            *self.status.borrow_mut() = st(want.mic, want.deaf);
            Ok(self.status.borrow().clone())
        }
    }

    fn fake() -> Fake {
        Fake { sent: RefCell::new(vec![]), status: RefCell::new(st(false, false)) }
    }

    const SOCK: &str = "/nonexistent/room.sock";

    #[test]
    fn a_mute_after_an_unmute_while_voice_reconnects_ends_muted_with_nothing_outstanding() {
        let mut v = up();
        v.set_desired(Some(true), None);
        let j1 = v.next_job(true).unwrap();
        // The first send hits a reconnect and fails; the person mutes meanwhile.
        v.set_desired(Some(false), None);
        v.finish(JobResult { job: j1, res: Err(VoiceError::Reconnecting) });
        assert!(v.view().pending && v.view().detail.contains("not applied"));
        v.retry_at = None; // the 250 ms timer elapsed
        let link = fake();
        let j2 = v.next_job(true).unwrap();
        let r2 = run_job(&link, Path::new(SOCK), j2);
        v.finish(r2);
        // What voice was last told is the muted state; the old unmute is never replayed.
        assert_eq!(link.sent.borrow().as_slice(), &[Desired { mic: false, deaf: false }]);
        assert!(!v.mic && !v.view().pending && v.settled());
        assert!(v.next_job(true).is_none_or(|j| !j.send), "nothing left to send");
    }

    #[test]
    fn two_requests_in_either_order_leave_the_last_write_at_voice() {
        let mut v = up();
        let link = fake();
        // Two ctl threads write under the lock; whichever lands last is `desired`.
        v.set_desired(Some(true), Some(true));
        v.set_desired(Some(false), None);
        let j = v.next_job(true).unwrap();
        let r = run_job(&link, Path::new(SOCK), j);
        v.finish(r);
        assert_eq!(link.sent.borrow().as_slice(), &[Desired { mic: false, deaf: true }], "full state, last write wins");
        assert!(v.settled());
    }

    #[test]
    fn a_request_made_during_a_send_is_sent_next_not_lost_and_not_marked_acked() {
        let mut v = up();
        let link = fake();
        v.set_desired(Some(true), None);
        let j1 = v.next_job(true).unwrap();
        v.set_desired(Some(false), None); // arrives while j1 is in flight
        let r1 = run_job(&link, Path::new(SOCK), j1);
        v.finish(r1);
        assert!(!v.settled(), "the reply was for the older gen");
        let j2 = v.next_job(true).unwrap();
        assert!(j2.send);
        let r2 = run_job(&link, Path::new(SOCK), j2);
        v.finish(r2);
        assert_eq!(link.sent.borrow().last(), Some(&Desired { mic: false, deaf: false }));
        assert!(v.settled() && !v.mic);
    }

    #[test]
    fn a_reply_for_an_old_process_is_ignored() {
        let mut v = up();
        v.set_desired(Some(true), None);
        let j = v.next_job(true).unwrap();
        // The process is replaced before the reply arrives.
        v.adopted = Some(std::process::id() as i32 + 1_000_000);
        let before = (v.mic, v.reported_gen);
        v.finish(JobResult { job: j, res: Ok(st(true, false)) });
        assert_eq!((v.mic, v.reported_gen), before, "the new process was not credited with the old reply");
    }

    #[test]
    fn a_stale_status_read_does_not_flip_reported_after_a_newer_ack() {
        let mut v = up();
        let link = fake();
        // A read is taken (voice says muted), then a request is sent and acknowledged.
        v.reported_gen = v.gen;
        let probe = v.next_job(true).unwrap();
        assert!(!probe.send);
        let stale = run_job(&link, Path::new(SOCK), probe); // reads mic=false
        v.set_desired(Some(true), None);
        let j = v.next_job(true).unwrap();
        let r = run_job(&link, Path::new(SOCK), j);
        v.finish(r); // acked: mic live
        v.finish(stale); // the old read arrives late
        assert!(v.mic, "the older read must not report muted while the mic is live");
        assert!(v.settled());
    }

    #[test]
    fn voice_muting_itself_is_not_undone_by_the_session() {
        let mut v = up();
        let link = fake();
        v.set_desired(Some(true), None);
        let j = v.next_job(true).unwrap();
        let r = run_job(&link, Path::new(SOCK), j);
        v.finish(r);
        assert!(v.mic);
        // Voice's safety rule mutes it with no request outstanding.
        *link.status.borrow_mut() = st(false, false);
        v.last_probe = None;
        let p = v.next_job(true).unwrap();
        assert!(!p.send);
        let r = run_job(&link, Path::new(SOCK), p);
        v.finish(r);
        assert!(!v.mic && !v.desired.mic && v.settled(), "adopted as the wanted state");
        v.last_probe = None;
        assert!(v.next_job(true).is_none_or(|j| !j.send), "nothing is sent to unmute it again");
    }

    #[test]
    fn a_live_mic_that_should_be_muted_is_sent_the_desired_state_again() {
        let mut v = up();
        let link = fake();
        *link.status.borrow_mut() = st(true, false); // voice is live, desired is muted
        v.last_probe = None;
        let p = v.next_job(true).unwrap();
        let r = run_job(&link, Path::new(SOCK), p);
        v.finish(r);
        let j = v.next_job(true).unwrap();
        assert!(j.send && !j.want.mic, "resend the muted state");
    }

    #[test]
    fn a_deafen_before_the_room_is_up_starts_voice_deaf_and_always_muted() {
        let mut v = Voice::new(true);
        v.set_desired(Some(true), Some(true)); // before any voice process exists
        let a = v.args(Path::new(SOCK), false);
        assert!(a.iter().any(|x| x == "--start-deaf"), "{a:?}");
        assert!(a.iter().any(|x| x == "--start-muted"), "{a:?}");
        // The start itself makes "muted" the desired mic and settles what it started with.
        v.started_as_desired();
        assert!(!v.desired.mic && v.desired.deaf && v.deaf && !v.mic);
        assert_eq!(v.reported_gen, v.gen);
    }

    #[test]
    fn a_toggle_with_no_voice_process_starts_nothing_and_takes_nothing_over() {
        let mut v = Voice::new(true);
        v.state = "busy"; // another program holds the lease
        let g = v.set_desired(Some(false), Some(true));
        assert_eq!(g, v.gen());
        assert!(v.next_job(true).is_none(), "no process: nothing to talk to");
        assert!(!v.running() && v.starts == 0, "no start");
        assert_eq!(v.state, "busy", "no takeover");
    }
}
