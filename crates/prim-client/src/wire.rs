//! Wire messages of the local API: `u32 LE length || JSON`. Mirrors
//! `prim_room::local` (keys lowercase hex, payloads base64).

use std::io::{Read, Write};

use data_encoding::BASE64;
use serde::{Deserialize, Serialize};

pub const MAX_MESSAGE: usize = 1 << 20;

pub fn b64(b: &[u8]) -> String {
    BASE64.encode(b)
}

pub fn unb64(s: &str) -> Result<Vec<u8>, String> {
    BASE64
        .decode(s.as_bytes())
        .map_err(|e| format!("bad base64: {e}"))
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Subscribe { req: u64, sub: u32, pattern: String },
    Unsubscribe { sub: u32 },
    Write {
        req: u64,
        /// "member" or "room".
        scope: String,
        name: String,
        #[serde(default)]
        live: bool,
        #[serde(default)]
        persistent: bool,
        /// base64.
        value: String,
    },
    RegisterChannel { req: u64, channel: u16 },
    UnregisterChannel { channel: u16 },
    Send {
        req: u64,
        /// "all", "coordinator" or a member key in hex.
        to: String,
        channel: u16,
        #[serde(default)]
        reliable: bool,
        payload: String,
    },
    SendDatagram {
        to: String,
        channel: u16,
        payload: String,
        #[serde(default)]
        max_age_ms: u32,
    },
    Status { req: u64 },
    CreateInvite {
        req: u64,
        #[serde(default)]
        expiry_in_ms: u64,
        #[serde(default)]
        relays: Vec<String>,
    },
    Grant {
        req: u64,
        kind: String,
        subject: String,
        #[serde(default)]
        expiry_in_ms: u64,
    },
    Revoke { req: u64, subject: String },
    Close { req: u64 },
    Reopen { req: u64 },
    Leave { req: u64 },
    /// Change the voice client's state and wait for its acknowledging report. `mic: true`
    /// is a live microphone, `deaf: true` deafened; fields left out are unchanged.
    VoiceControl {
        req: u64,
        #[serde(default)]
        mic: Option<bool>,
        #[serde(default)]
        deaf: Option<bool>,
        #[serde(default)]
        tray_live: Option<bool>,
    },
    /// The voice client's last reported state.
    VoiceStatus { req: u64 },
    /// Stop the daemon (it replies Ok, saves state and exits).
    Shutdown { req: u64 },
    Drain {
        sub: u32,
        #[serde(default = "default_drain_max")]
        max: usize,
    },
}

fn default_drain_max() -> usize {
    64
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReplyJson {
    Ok,
    Error { code: String },
    Sending { msg_id: u64, targets: Vec<String> },
    /// Raw JSON, so older callers keep indexing it; [`StatusJson::from_value`] types it.
    Status { status: serde_json::Value },
    Invite { token: String },
    Voice { voice: VoiceStatus },
}

/// The voice client's own report (mirrors `prim_room::rt::VoiceStatusJson`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct VoiceStatus {
    /// A voice client holds the lease; false: every other field but `tracking_held` is zero.
    pub voice_running: bool,
    pub session: u64,
    /// What peers see in `member/<id>/voice`.
    pub incarnation: u32,
    /// Not deafened (wire flag bit0).
    pub listening: bool,
    /// The microphone is actually live (wire flag bit1).
    pub mic: bool,
    pub muted: bool,
    pub deaf: bool,
    pub tray_live: bool,
    pub reported: bool,
    pub tracking_held: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct VersionJson {
    pub generation: u32,
    pub term: u32,
    pub seq: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UpdateJson {
    Reset,
    Value {
        path: String,
        writer: String,
        version: VersionJson,
        /// "durable" or "live".
        class: String,
        /// base64.
        value: String,
    },
    Removed { path: String },
    SnapshotDone,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Reply { req: u64, reply: ReplyJson },
    Updates { sub: u32, updates: Vec<UpdateJson> },
    Message { from: String, channel: u16, payload: String },
    Datagram { from: String, channel: u16, payload: String, age_ms: u32 },
    MessageResult {
        msg_id: u64,
        to: String,
        /// "delivered", "no_channel", "not_member", "too_large", "lost" or "offline".
        status: String,
    },
    Member {
        key: String,
        /// "joined", "online", "offline", "unresponsive" or "removed".
        what: String,
    },
    Coordinator {
        change: String,
        generation: u32,
        term: u32,
        holder: String,
    },
    RoomClosed { generation: u32 },
    RoomOpened { generation: u32 },
    Removed,
    Error { message: String },
}

/// Encode one framed message.
pub fn frame<T: Serialize>(v: &T) -> std::io::Result<Vec<u8>> {
    let body = serde_json::to_vec(v).map_err(std::io::Error::other)?;
    if body.len() > MAX_MESSAGE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "message too large",
        ));
    }
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

pub fn write_msg<W: Write, T: Serialize>(w: &mut W, v: &T) -> std::io::Result<()> {
    w.write_all(&frame(v)?)?;
    w.flush()
}

/// Next raw message; `Ok(None)` on a clean EOF at a message boundary.
pub fn read_raw<R: Read>(r: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let n = u32::from_le_bytes(len) as usize;
    if n > MAX_MESSAGE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    let mut body = vec![0u8; n];
    r.read_exact(&mut body)?;
    Ok(Some(body))
}

// ---- typed status -------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct MemberJson {
    pub key: String,
    pub keyholder: bool,
    pub inviter: bool,
    pub connected: bool,
    pub online: bool,
    pub since_ms: Option<u64>,
    pub fresh: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct CoordinatorJson {
    pub generation: u32,
    pub term: u32,
    pub holder: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct RouteJson {
    pub peer: String,
    pub conn: u64,
    pub outbound: bool,
    pub path: String,
    pub max_datagram_size: Option<usize>,
    pub rtt_ms: Option<u64>,
    pub datagrams_sent: u64,
    pub datagrams_dropped_age: u64,
    pub datagrams_dropped_queue: u64,
    pub datagrams_dropped_transport: u64,
    pub datagrams_too_large: u64,
    pub datagrams_dropped_in: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct HolderJson {
    pub role: String,
    pub session: u64,
    pub incarnation: u32,
}

/// The realtime lane's summary. `drops` and the histograms stay raw JSON.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct RtJson {
    #[serde(default)]
    pub holders: Vec<HolderJson>,
    #[serde(default)]
    pub queue_depth: usize,
    #[serde(default)]
    pub sent: u64,
    #[serde(default)]
    pub received: u64,
    #[serde(default)]
    pub stalls: u64,
    #[serde(default)]
    pub drops: serde_json::Value,
}

impl StatusJson {
    pub fn from_value(v: &serde_json::Value) -> Option<StatusJson> {
        serde_json::from_value(v.clone()).ok()
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct StatusJson {
    /// "none", "pending" (dialing the inviter) or "refused" (see `join_refused`).
    #[serde(default)]
    pub join: String,
    /// `not_member`, `invite_invalid`, `closed`, `revoked`, `wrong_room`, `version`, `unsupported`.
    #[serde(default)]
    pub join_refused: Option<String>,
    /// The room's name (`found --meta`), when it is text.
    #[serde(default)]
    pub name: Option<String>,
    pub room: Option<String>,
    pub me: String,
    /// "dead", "open" or "closed".
    pub lifecycle: String,
    pub generation: Option<u32>,
    pub coordinator: Option<CoordinatorJson>,
    pub members: Vec<MemberJson>,
    #[serde(default)]
    pub records: usize,
    #[serde(default)]
    pub durable_bytes_room: usize,
    #[serde(default)]
    pub durable_bytes_mine: usize,
    #[serde(default)]
    pub routes: Vec<RouteJson>,
    #[serde(default)]
    pub rt: Option<RtJson>,
}
