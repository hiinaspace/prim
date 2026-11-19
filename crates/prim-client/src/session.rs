//! The session supervisor's control protocol, and a blocking client for it.
//!
//! `prim session` (crate `prim-session`) is the one background process that starts and
//! keeps `prim room` and `prim voice`. Godot (through `PrimClient`), `prim ctl session`
//! and a later tray only send it requests over `session.sock`: `u32 LE length || JSON`
//! each way, one request then one reply per connection.
//!
//! This file is the protocol; the server lives in `crates/prim-session`.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::wire::{read_raw, write_msg};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum SessionRequest {
    /// Where things stand. Never changes anything.
    Status,
    /// Found a room (starting `prim room found`). `name` is the room's readable name.
    Create {
        #[serde(default)]
        name: String,
        /// Extra `prim room` arguments (--relay, --bind, --peer-addr ...).
        #[serde(default)]
        args: Vec<String>,
    },
    /// Join with an invite token (starting `prim room join`).
    Join {
        token: String,
        #[serde(default)]
        args: Vec<String>,
    },
    /// Resume a saved room (hex id).
    Resume {
        room: String,
        #[serde(default)]
        args: Vec<String>,
    },
    /// Leave the room and stop its daemon. Voice stops too.
    Leave,
    /// Give up on a start that has not produced a room (a join nobody answers).
    Cancel,
    /// Turn the voice client on or off. On starts it muted and hearing.
    Voice { on: bool },
    /// Microphone on/off. Starts voice if it is wanted but not running.
    SetMic { on: bool },
    /// Silence what you hear on/off.
    SetDeafen { on: bool },
    /// Stop voice, the room daemon and the session itself.
    Quit,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct VoiceState {
    /// A voice client process is running.
    pub running: bool,
    /// Someone asked for voice (it runs once there is a room).
    pub wanted: bool,
    pub mic: bool,
    pub deaf: bool,
    /// "off", "running", "restarting", "busy" (another program holds the lease) or "failed".
    pub state: String,
    /// What went wrong, for "failed" and "restarting".
    #[serde(default)]
    pub detail: String,
    /// Process id of the voice client; changes whenever voice is replaced.
    #[serde(default)]
    pub pid: Option<u32>,
    /// How many voice clients this session has started (adoption does not count).
    pub starts: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct SessionStatus {
    pub pid: u32,
    /// "none" (no daemon), "starting", "up" or "failed" (see `error`).
    pub room: String,
    /// What the pending start is doing: "found", "join", "resume", or "attached" when the
    /// daemon was already running when the session came up.
    pub kind: String,
    #[serde(default)]
    pub error: String,
    pub room_socket: String,
    pub voice: VoiceState,
    /// A tray icon is live in the session process (round T1; always false until then).
    /// When false, a front end that closes must leave and quit rather than leave a live
    /// microphone with nothing visible.
    #[serde(default)]
    pub tray_live: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum SessionReply {
    Ok { status: SessionStatus },
    Error { message: String },
}

fn io_err(m: impl ToString) -> std::io::Error {
    std::io::Error::other(m.to_string())
}

/// Blocking client for one session socket.
#[derive(Clone, Debug)]
pub struct SessionClient {
    pub socket: PathBuf,
    pub timeout: Duration,
}

impl SessionClient {
    pub fn new(room_socket: &Path) -> Self {
        Self {
            socket: crate::paths::session_socket(room_socket),
            timeout: Duration::from_secs(40),
        }
    }

    /// Is a session answering?
    pub fn alive(&self) -> bool {
        self.call(&SessionRequest::Status).is_ok()
    }

    pub fn call(&self, req: &SessionRequest) -> std::io::Result<SessionStatus> {
        let mut s = UnixStream::connect(&self.socket)?;
        s.set_read_timeout(Some(self.timeout))?;
        s.set_write_timeout(Some(Duration::from_secs(5)))?;
        write_msg(&mut s, req)?;
        let body = read_raw(&mut s)?.ok_or_else(|| io_err("session closed the connection"))?;
        match serde_json::from_slice::<SessionReply>(&body).map_err(io_err)? {
            SessionReply::Ok { status } => Ok(status),
            SessionReply::Error { message } => Err(io_err(message)),
        }
    }

    pub fn status(&self) -> std::io::Result<SessionStatus> {
        self.call(&SessionRequest::Status)
    }

    /// Make sure a session is running: attach to the one that is, else start `prim session`
    /// detached (its own process group, output to the session log) and wait for its
    /// socket. A second caller therefore adopts the first one's session.
    pub fn ensure_running(&self, room_socket: &Path) -> std::io::Result<SessionStatus> {
        if let Ok(st) = self.status() {
            return Ok(st);
        }
        spawn_session(room_socket)?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match self.status() {
                Ok(st) => return Ok(st),
                Err(e) if Instant::now() > deadline => {
                    return Err(io_err(format!("the session did not start: {e}")))
                }
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }
}

/// Start `prim session --socket ...` detached. Does not wait.
pub fn spawn_session(room_socket: &Path) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(crate::paths::log_dir().join("session.log"))?;
    let mut c = Command::new(crate::paths::prim_exe());
    c.arg("session")
        .arg(format!("--socket={}", room_socket.display()))
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0);
    let mut child = c.spawn()?;
    // Reap it when it exits so a failed start does not leave a zombie in this process.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

// Re-exported so callers need only this module for the byte-level helpers the server uses.
pub fn read_request<R: Read>(r: &mut R) -> std::io::Result<Option<SessionRequest>> {
    match read_raw(r)? {
        None => Ok(None),
        Some(b) => serde_json::from_slice(&b).map(Some).map_err(io_err),
    }
}

pub fn write_reply<W: Write>(w: &mut W, reply: &SessionReply) -> std::io::Result<()> {
    write_msg(w, reply)
}
