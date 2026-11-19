//! The session supervisor behind `prim session`.
//!
//! One process per user (per room socket) that starts and keeps the programs a Prim
//! client needs, so no GUI has to: `prim room` (the daemon) and `prim voice`. It answers a
//! small control protocol on `session.sock` (`prim_client::session`):
//! create/join/resume/leave/cancel, voice on/off, mic and deafen, status, quit.
//!
//! - A second `prim session` finds the socket answering and just prints the first one's
//!   status (single instance), so "start one if none is running" is safe from anywhere.
//! - Everything it spawns gets its own process group and outlives it: quitting Godot or
//!   even killing the session leaves the room and voice running, and a new session
//!   adopts both (it finds the daemon on `room.sock` and the voice client in `/proc`).
//! - Mic and deafen are requests to the running voice client ([`Control::set_voice_state`],
//!   round VC): nothing restarts, the incarnation stays. Voice starts only when absent.

mod daemon;
mod procs;
pub mod tray;
mod voice;

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use prim_client::paths;
use prim_client::session::{
    read_request, write_reply, SessionClient, SessionReply, SessionRequest, SessionStatus,
};

use daemon::{Room, RoomKind};
use voice::Voice;

pub struct Options {
    pub room_socket: PathBuf,
    /// Start voice automatically once there is a room (muted). Off for headless boxes.
    pub voice: bool,
}

pub(crate) struct State {
    pub room_socket: PathBuf,
    pub room: Room,
    pub voice: Voice,
    pub quit: bool,
}

/// The handle the control socket, the tray and tests share.
#[derive(Clone)]
pub struct Control {
    state: Arc<Mutex<State>>,
    /// Signalled when a request is written and when voice acknowledges one: the supervisor
    /// (the only sender to the voice client) wakes on it, request callers wait on it.
    changed: Arc<Condvar>,
}

impl Control {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn status(&self) -> SessionStatus {
        let s = self.lock();
        status_of(&s)
    }

    /// Start a room daemon (`kind`) and wait for its socket. Errors if one is running.
    pub fn start_room(&self, kind: RoomKind, args: Vec<String>) -> Result<SessionStatus, String> {
        {
            let mut s = self.lock();
            let sock = s.room_socket.clone();
            s.room.start(kind, args, &sock)?;
        }
        let deadline = Instant::now() + Duration::from_secs(26);
        loop {
            std::thread::sleep(Duration::from_millis(100));
            let mut s = self.lock();
            tick(&mut s);
            match s.room.phase() {
                "up" => return Ok(status_of(&s)),
                "failed" => return Err(s.room.error().to_string()),
                "none" => return Err("the room daemon stopped".into()),
                _ if Instant::now() > deadline => {
                    s.room.abort();
                    return Err("the room daemon did not start in time".into());
                }
                _ => {}
            }
        }
    }

    pub fn leave(&self) -> Result<SessionStatus, String> {
        let mut s = self.lock();
        let sock = s.room_socket.clone();
        s.room.leave_and_stop(&sock);
        { let sock = s.room_socket.clone(); s.voice.stop_at(&sock); }
        Ok(status_of(&s))
    }

    pub fn cancel(&self) -> Result<SessionStatus, String> {
        let mut s = self.lock();
        let sock = s.room_socket.clone();
        s.room.stop(&sock);
        { let sock = s.room_socket.clone(); s.voice.stop_at(&sock); }
        Ok(status_of(&s))
    }

    pub fn set_voice_wanted(&self, on: bool) -> Result<SessionStatus, String> {
        let mut s = self.lock();
        let (room_up, sock) = (s.room.phase() == "up", s.room_socket.clone());
        if on {
            s.voice.turn_on(room_up, &sock);
        } else {
            s.voice.wanted = false;
            { let sock = s.room_socket.clone(); s.voice.stop_at(&sock); }
        }
        tick(&mut s);
        Ok(status_of(&s))
    }

    /// The one place mic and deafen change. It writes the desired state and wakes the
    /// supervisor, which alone talks to the voice client (always sending the whole desired
    /// state, so requests cannot overtake each other). The call waits up to 3 s for voice to
    /// acknowledge; the returned status shows what voice reports, and `voice.pending` says
    /// whether the change is still being applied. A toggle never starts or replaces voice.
    pub fn set_voice_state(&self, mic: Option<bool>, deaf: Option<bool>) -> Result<SessionStatus, String> {
        let mut s = self.lock();
        s.voice.set_desired(mic, deaf);
        self.changed.notify_all();
        let end = Instant::now() + Duration::from_secs(3);
        while !s.voice.settled() {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            s = self.changed.wait_timeout(s, left).unwrap_or_else(|e| e.into_inner()).0;
        }
        Ok(status_of(&s))
    }

    pub fn quit(&self) -> SessionStatus {
        let mut s = self.lock();
        let sock = s.room_socket.clone();
        { let sock = s.room_socket.clone(); s.voice.stop_at(&sock); }
        s.room.stop(&sock);
        s.quit = true;
        status_of(&s)
    }
}

fn status_of(s: &State) -> SessionStatus {
    SessionStatus {
        pid: std::process::id(),
        room: s.room.phase().to_string(),
        kind: s.room.kind_name().to_string(),
        error: s.room.error().to_string(),
        room_socket: s.room_socket.display().to_string(),
        voice: s.voice.view(),
        tray_live: tray::live(),
    }
}

/// One supervision step: notice exits, relaunch what should run.
pub(crate) fn tick(s: &mut State) {
    let sock = s.room_socket.clone();
    s.room.poll(&sock);
    let room_up = s.room.phase() == "up";
    s.voice.poll(room_up, &sock);
}

/// Run the supervisor until `Quit`. If another session already answers on the socket,
/// print its status and return (single instance).
pub fn run(opts: Options) -> Result<()> {
    let ctl_path = paths::session_socket(&opts.room_socket);
    if let Ok(st) = SessionClient::new(&opts.room_socket).status() {
        println!("session already running (pid {}), room {}", st.pid, st.room);
        return Ok(());
    }
    // One session per socket even when two start at once: whoever holds the lock binds.
    let _lock = match lock_single(&ctl_path) {
        Some(l) => l,
        None => {
            // Lost the race; the winner is binding. Report it if it answers.
            for _ in 0..30 {
                if let Ok(st) = SessionClient::new(&opts.room_socket).status() {
                    println!("session already running (pid {}), room {}", st.pid, st.room);
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            anyhow::bail!("another session holds {}.lock but does not answer", ctl_path.display());
        }
    };
    let listener = bind(&ctl_path)?;
    let mut room = Room::new();
    let mut voice = Voice::new(opts.voice);
    // Adopt whatever an earlier session (or Godot of an older build) left running.
    room.adopt(&opts.room_socket);
    voice.adopt(&voice::Daemon, &opts.room_socket);
    let state = Arc::new(Mutex::new(State {
        room_socket: opts.room_socket.clone(),
        room,
        voice,
        quit: false,
    }));
    let control = Control { state: state.clone(), changed: Arc::new(Condvar::new()) };
    println!("session {} on {}", std::process::id(), ctl_path.display());
    {
        let c = control.clone();
        std::thread::Builder::new()
            .name("supervise".into())
            .spawn(move || loop {
                // The only thread that talks to the voice client: plan under the lock,
                // talk unlocked (status calls, ctl and accept never wait for voice), fold
                // the answer in, and go round again at once if a newer request arrived.
                let job = {
                    let mut s = c.lock();
                    tick(&mut s);
                    let room_up = s.room.phase() == "up";
                    let sock = s.room_socket.clone();
                    s.voice.next_job(room_up).map(|j| (sock, j))
                };
                if let Some((sock, job)) = job {
                    let r = voice::run_job(&voice::Daemon, &sock, job);
                    c.lock().voice.finish(r);
                    c.changed.notify_all();
                    continue;
                }
                let s = c.lock();
                let _ = c.changed.wait_timeout(s, Duration::from_millis(250));
            })?;
    }
    tray::start(control.clone());
    listener.set_nonblocking(true)?;
    loop {
        if control.lock().quit {
            // Let the reply to Quit flush.
            std::thread::sleep(Duration::from_millis(200));
            break;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let c = control.clone();
                std::thread::spawn(move || {
                    let _ = serve_one(stream, &c);
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(30))
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    let _ = std::fs::remove_file(&ctl_path);
    Ok(())
}

/// An exclusive advisory lock on `<session.sock>.lock`, held for the life of the process.
fn lock_single(ctl: &Path) -> Option<std::fs::File> {
    use std::os::fd::AsRawFd;
    if let Some(dir) = ctl.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(ctl.with_extension("sock.lock"))
        .ok()?;
    // SAFETY: flock on a descriptor we own.
    (unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0).then_some(f)
}

fn bind(path: &Path) -> Result<UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    if path.exists() {
        // Only here when nobody answered a status request: the file is stale.
        if UnixStream::connect(path).is_ok() {
            anyhow::bail!("something already listens on {}", path.display());
        }
        std::fs::remove_file(path)?;
    }
    let l = UnixListener::bind(path).with_context(|| path.display().to_string())?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(l)
}

fn serve_one(mut stream: UnixStream, c: &Control) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let Some(req) = read_request(&mut stream)? else {
        return Ok(());
    };
    let result = handle(c, req);
    let reply = match result {
        Ok(status) => SessionReply::Ok { status },
        Err(message) => SessionReply::Error { message },
    };
    write_reply(&mut stream, &reply)
}

fn handle(c: &Control, req: SessionRequest) -> Result<SessionStatus, String> {
    use SessionRequest::*;
    match req {
        Status => Ok(c.status()),
        Create { name, args } => c.start_room(RoomKind::Found { name }, args),
        Join { token, args } => {
            if !token.trim().starts_with("prim1") {
                return Err("That is not an invite (it should start with prim1).".into());
            }
            c.start_room(RoomKind::Join(token.trim().to_string()), args)
        }
        Resume { room, args } => c.start_room(RoomKind::Resume(room), args),
        Leave => c.leave(),
        Cancel => c.cancel(),
        Voice { on } => c.set_voice_wanted(on),
        SetMic { on } => c.set_voice_state(Some(on), None),
        SetDeafen { on } => c.set_voice_state(None, Some(on)),
        Quit => Ok(c.quit()),
    }
}
