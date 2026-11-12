//! The provisional local API (CONTRACTS.md section 8 names the client session).
//!
//! A Unix socket, default `$XDG_RUNTIME_DIR/prim/room.sock`, in a 0700 directory. Every
//! message is `u32 LE length || JSON`. Requests mirror `LocalCommand` (plus `drain`);
//! events mirror `LocalEvent` and `Update`. Keys are lowercase hex, payloads base64.
//! One socket connection is one `ClientId`: `Attach` on accept, `Detach` on close.
//!
//! Coalescing: when the core says `Ready{sub}` the client task drains that subscription
//! only between socket writes, so a client that reads slowly holds at most one batch
//! and the core keeps one value per key for it.

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use data_encoding::BASE64;
use room_core::api::{
    CoordinatorEvent, ErrorCode, LocalCommand, LocalEvent, MemberEvent, MessageStatus, Reply, To,
    Update,
};
use room_core::frame::AckStatus;
use room_core::grants::Lifecycle;
use room_core::ids::Key;
use room_core::record::Kind;
use room_core::staterec::{Class, Lifetime, Scope};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use crate::driver::{ClientMsg, DriverHandle, RouteInfo};

pub const MAX_MESSAGE: usize = 1 << 20;
/// Updates written per drain.
pub const DRAIN_BATCH: usize = 64;

pub fn default_socket_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d).join("prim").join("room.sock"),
        None => PathBuf::from(format!("/tmp/prim-{}/room.sock", current_uid())),
    }
}

fn current_uid() -> String {
    // No libc dependency: the owner of /proc/self is this process's uid.
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self")
        .map(|m| m.uid().to_string())
        .unwrap_or_else(|_| "0".into())
}

// ---- wire types -------------------------------------------------------------------

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Subscribe {
        req: u64,
        sub: u32,
        pattern: String,
    },
    Unsubscribe {
        sub: u32,
    },
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
    RegisterChannel {
        req: u64,
        channel: u16,
    },
    UnregisterChannel {
        channel: u16,
    },
    Send {
        req: u64,
        /// "all", "coordinator" or a member key in hex.
        to: String,
        channel: u16,
        #[serde(default)]
        reliable: bool,
        /// base64.
        payload: String,
    },
    SendDatagram {
        to: String,
        channel: u16,
        payload: String,
        #[serde(default)]
        max_age_ms: u32,
    },
    Status {
        req: u64,
    },
    CreateInvite {
        req: u64,
        /// Lifetime from now in ms (0 = no expiry). The core's records carry an absolute
        /// Unix time, so the daemon adds its wall clock before handing the command over.
        #[serde(default)]
        expiry_in_ms: u64,
        #[serde(default)]
        relays: Vec<String>,
    },
    Grant {
        req: u64,
        /// "keyholder", "inviter" or "member".
        kind: String,
        subject: String,
        /// Lifetime from now in ms (0 = no expiry).
        #[serde(default)]
        expiry_in_ms: u64,
    },
    Revoke {
        req: u64,
        subject: String,
    },
    Retire {
        req: u64,
        subject: String,
        #[serde(default)]
        keep_all: bool,
    },
    Close {
        req: u64,
    },
    Reopen {
        req: u64,
    },
    Leave {
        req: u64,
    },
    /// Pull pending updates of a subscription now (the daemon also pushes them).
    Drain {
        sub: u32,
        #[serde(default = "default_drain_max")]
        max: usize,
    },
}

fn default_drain_max() -> usize {
    DRAIN_BATCH
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReplyJson {
    Ok,
    Error { code: String },
    Sending { msg_id: u64, targets: Vec<String> },
    Status { status: StatusJson },
    Invite { token: String },
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
    Removed {
        path: String,
    },
    SnapshotDone,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct MemberJson {
    pub key: String,
    pub keyholder: bool,
    pub inviter: bool,
    pub connected: bool,
    /// The member's presence record says online.
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
    /// "direct", "relay", "mem" or "unknown".
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

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct StatusJson {
    pub room: Option<String>,
    pub me: String,
    /// "dead", "open" or "closed".
    pub lifecycle: String,
    pub generation: Option<u32>,
    pub coordinator: Option<CoordinatorJson>,
    pub members: Vec<MemberJson>,
    pub records: usize,
    pub durable_bytes_room: usize,
    pub durable_bytes_mine: usize,
    /// One entry per live peer connection.
    pub routes: Vec<RouteJson>,
    /// The realtime voice lane, when the daemon runs one.
    #[serde(default)]
    pub rt: Option<Box<crate::rt::RtJson>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Reply {
        req: u64,
        reply: ReplyJson,
    },
    Updates {
        sub: u32,
        updates: Vec<UpdateJson>,
    },
    Message {
        from: String,
        channel: u16,
        payload: String,
    },
    Datagram {
        from: String,
        channel: u16,
        payload: String,
        age_ms: u32,
    },
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
        /// "changed", "left" or "unresponsive".
        change: String,
        generation: u32,
        term: u32,
        holder: String,
    },
    RoomClosed {
        generation: u32,
    },
    RoomOpened {
        generation: u32,
    },
    Removed,
    Error {
        message: String,
    },
}

pub fn b64(b: &[u8]) -> String {
    BASE64.encode(b)
}

pub fn unb64(s: &str) -> Result<Vec<u8>, String> {
    BASE64
        .decode(s.as_bytes())
        .map_err(|e| format!("bad base64: {e}"))
}

fn key_from_hex(s: &str) -> Result<Key, String> {
    Key::from_hex(s).ok_or_else(|| format!("bad key {s:?}"))
}

// ---- conversions ------------------------------------------------------------------

impl Request {
    /// `Ok(None)` for `Drain`, which isn't a core command.
    pub fn to_command(&self, now_unix_ms: u64) -> Result<Option<LocalCommand>, String> {
        Ok(Some(match self {
            Request::Subscribe { req, sub, pattern } => LocalCommand::Subscribe {
                req: *req,
                sub: *sub,
                pattern: pattern.clone(),
            },
            Request::Unsubscribe { sub } => LocalCommand::Unsubscribe { sub: *sub },
            Request::Write {
                req,
                scope,
                name,
                live,
                persistent,
                value,
            } => LocalCommand::Write {
                req: *req,
                scope: match scope.as_str() {
                    "member" => Scope::Member,
                    "room" => Scope::Room,
                    s => return Err(format!("bad scope {s:?}")),
                },
                name: name.clone(),
                class: if *live { Class::Live } else { Class::Durable },
                lifetime: if *persistent {
                    Lifetime::Persistent
                } else {
                    Lifetime::PresenceBound
                },
                value: unb64(value)?,
            },
            Request::RegisterChannel { req, channel } => LocalCommand::RegisterChannel {
                req: *req,
                channel: *channel,
            },
            Request::UnregisterChannel { channel } => {
                LocalCommand::UnregisterChannel { channel: *channel }
            }
            Request::Send {
                req,
                to,
                channel,
                reliable,
                payload,
            } => LocalCommand::Send {
                req: *req,
                to: parse_to(to)?,
                channel: *channel,
                reliable: *reliable,
                payload: unb64(payload)?,
            },
            Request::SendDatagram {
                to,
                channel,
                payload,
                max_age_ms,
            } => LocalCommand::SendDatagram {
                to: parse_to(to)?,
                channel: *channel,
                payload: unb64(payload)?,
                max_age_ms: *max_age_ms,
            },
            Request::Status { req } => LocalCommand::Status { req: *req },
            Request::CreateInvite {
                req,
                expiry_in_ms,
                relays,
            } => LocalCommand::CreateInvite {
                req: *req,
                expiry_ms: absolute(now_unix_ms, *expiry_in_ms),
                relays: relays.clone(),
            },
            Request::Grant {
                req,
                kind,
                subject,
                expiry_in_ms,
            } => LocalCommand::Grant {
                req: *req,
                kind: match kind.as_str() {
                    "keyholder" => Kind::Keyholder,
                    "inviter" => Kind::Inviter,
                    "member" => Kind::Member,
                    s => return Err(format!("bad grant kind {s:?}")),
                },
                subject: key_from_hex(subject)?,
                expiry_ms: absolute(now_unix_ms, *expiry_in_ms),
            },
            Request::Revoke { req, subject } => LocalCommand::Revoke {
                req: *req,
                subject: key_from_hex(subject)?,
            },
            Request::Retire {
                req,
                subject,
                keep_all,
            } => LocalCommand::Retire {
                req: *req,
                subject: key_from_hex(subject)?,
                keep_all: *keep_all,
            },
            Request::Close { req } => LocalCommand::Close { req: *req },
            Request::Reopen { req } => LocalCommand::Reopen { req: *req },
            Request::Leave { req } => LocalCommand::Leave { req: *req },
            Request::Drain { .. } => return Ok(None),
        }))
    }
}

fn absolute(now_unix_ms: u64, in_ms: u64) -> u64 {
    if in_ms == 0 {
        0
    } else {
        now_unix_ms.saturating_add(in_ms)
    }
}

fn parse_to(s: &str) -> Result<To, String> {
    Ok(match s {
        "all" => To::All,
        "coordinator" => To::Coordinator,
        k => To::Member(key_from_hex(k)?),
    })
}

fn error_code(c: &ErrorCode) -> &'static str {
    match c {
        ErrorCode::NotAttached => "not_attached",
        ErrorCode::NoRoom => "no_room",
        ErrorCode::BadPattern => "bad_pattern",
        ErrorCode::BadName => "bad_name",
        ErrorCode::TooLarge => "too_large",
        ErrorCode::OverBudget => "over_budget",
        ErrorCode::NotCoordinator => "not_coordinator",
        ErrorCode::NoCoordinator => "no_coordinator",
        ErrorCode::NotAuthorized => "not_authorized",
        ErrorCode::Offline => "offline",
        ErrorCode::Closed => "closed",
        ErrorCode::Unknown => "unknown",
    }
}

fn route_json(r: &RouteInfo) -> RouteJson {
    RouteJson {
        peer: r.peer.hex(),
        conn: r.conn,
        outbound: r.outbound,
        path: r.route.kind.as_str().into(),
        max_datagram_size: r.route.max_datagram_size,
        rtt_ms: r.route.rtt_ms,
        datagrams_sent: r.datagrams_sent,
        datagrams_dropped_age: r.datagrams_dropped_age,
        datagrams_dropped_queue: r.datagrams_dropped_queue,
        datagrams_dropped_transport: r.stats.datagrams_dropped_out,
        datagrams_too_large: r.stats.datagrams_too_large,
        datagrams_dropped_in: r.stats.datagrams_dropped_in,
    }
}

pub fn update_json(u: &Update) -> UpdateJson {
    match u {
        Update::Reset => UpdateJson::Reset,
        Update::Value {
            path,
            writer,
            version,
            class,
            value,
        } => UpdateJson::Value {
            path: path.clone(),
            writer: writer.hex(),
            version: VersionJson {
                generation: version.generation,
                term: version.term,
                seq: version.seq,
            },
            class: match class {
                Class::Durable => "durable".into(),
                Class::Live => "live".into(),
            },
            value: b64(value),
        },
        Update::Removed { path } => UpdateJson::Removed { path: path.clone() },
        Update::SnapshotDone => UpdateJson::SnapshotDone,
    }
}

fn message_status(s: &MessageStatus) -> &'static str {
    match s {
        MessageStatus::Acked(AckStatus::Delivered) => "delivered",
        MessageStatus::Acked(AckStatus::NoChannel) => "no_channel",
        MessageStatus::Acked(AckStatus::NotMember) => "not_member",
        MessageStatus::Acked(AckStatus::TooLarge) => "too_large",
        MessageStatus::Lost => "lost",
        MessageStatus::Offline => "offline",
    }
}

/// Convert a core event. `Ready` has no wire form (the client task drains instead).
pub fn event_json(msg: &ClientMsg) -> Option<Event> {
    Some(match &msg.event {
        LocalEvent::Reply { req, reply } => Event::Reply {
            req: *req,
            reply: match reply {
                Reply::Ok => ReplyJson::Ok,
                Reply::Error(c) => ReplyJson::Error {
                    code: error_code(c).into(),
                },
                Reply::Sending { msg_id, targets } => ReplyJson::Sending {
                    msg_id: *msg_id,
                    targets: targets.iter().map(Key::hex).collect(),
                },
                Reply::Status(s) => {
                    let (lifecycle, generation) = match s.lifecycle {
                        Lifecycle::Dead => ("dead", None),
                        Lifecycle::Open(g) => ("open", Some(g)),
                        Lifecycle::Closed(g) => ("closed", Some(g)),
                    };
                    ReplyJson::Status {
                        status: StatusJson {
                            room: s.room.map(|r| r.hex()),
                            me: s.me.hex(),
                            lifecycle: lifecycle.into(),
                            generation,
                            coordinator: s.coordinator.map(|(e, k)| CoordinatorJson {
                                generation: e.generation,
                                term: e.term,
                                holder: k.hex(),
                            }),
                            members: s
                                .members
                                .iter()
                                .map(|m| MemberJson {
                                    key: m.key.hex(),
                                    keyholder: m.keyholder,
                                    inviter: m.inviter,
                                    connected: m.connected,
                                    online: m.presence.is_some(),
                                    since_ms: m.presence.as_ref().map(|p| p.since_ms),
                                    fresh: m.fresh,
                                })
                                .collect(),
                            records: s.records,
                            durable_bytes_room: s.durable_bytes_room,
                            durable_bytes_mine: s.durable_bytes_mine,
                            routes: msg
                                .routes
                                .as_deref()
                                .unwrap_or(&[])
                                .iter()
                                .map(route_json)
                                .collect(),
                            rt: msg.rt.clone(),
                        },
                    }
                }
                Reply::Invite { token } => ReplyJson::Invite {
                    token: token.clone(),
                },
            },
        },
        LocalEvent::Ready { .. } => return None,
        LocalEvent::Message {
            from,
            channel,
            payload,
        } => Event::Message {
            from: from.hex(),
            channel: *channel,
            payload: b64(payload),
        },
        LocalEvent::Datagram {
            from,
            channel,
            payload,
            age_ms,
        } => Event::Datagram {
            from: from.hex(),
            channel: *channel,
            payload: b64(payload),
            age_ms: *age_ms,
        },
        LocalEvent::MessageResult { msg_id, to, status } => Event::MessageResult {
            msg_id: *msg_id,
            to: to.hex(),
            status: message_status(status).into(),
        },
        LocalEvent::Member { key, event } => Event::Member {
            key: key.hex(),
            what: match event {
                MemberEvent::Joined => "joined",
                MemberEvent::Online => "online",
                MemberEvent::Offline => "offline",
                MemberEvent::Unresponsive => "unresponsive",
                MemberEvent::Removed => "removed",
            }
            .into(),
        },
        LocalEvent::Coordinator(c) => {
            let (change, epoch, holder) = match c {
                CoordinatorEvent::Changed { epoch, holder } => ("changed", epoch, holder),
                CoordinatorEvent::Left { epoch, holder } => ("left", epoch, holder),
                CoordinatorEvent::Unresponsive { epoch, holder } => ("unresponsive", epoch, holder),
            };
            Event::Coordinator {
                change: change.into(),
                generation: epoch.generation,
                term: epoch.term,
                holder: holder.hex(),
            }
        }
        LocalEvent::RoomClosed { generation } => Event::RoomClosed {
            generation: *generation,
        },
        LocalEvent::RoomOpened { generation } => Event::RoomOpened {
            generation: *generation,
        },
        LocalEvent::Removed => Event::Removed,
    })
}

// ---- framing ----------------------------------------------------------------------

pub async fn write_msg<W: AsyncWrite + Unpin, T: Serialize>(
    w: &mut W,
    v: &T,
) -> std::io::Result<()> {
    let body = serde_json::to_vec(v).map_err(std::io::Error::other)?;
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    w.write_all(&out).await?;
    w.flush().await
}

/// Next raw message; `Ok(None)` on a clean EOF at a message boundary.
pub async fn read_raw<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
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
    r.read_exact(&mut body).await?;
    Ok(Some(body))
}

// ---- server -----------------------------------------------------------------------

/// Bind the socket: create its directory (0700), replace a stale socket file, refuse
/// to steal a live one.
pub async fn bind(path: &Path) -> std::io::Result<UnixListener> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    if path.exists() {
        if UnixStream::connect(path).await.is_ok() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                format!("a daemon is already listening on {}", path.display()),
            ));
        }
        std::fs::remove_file(path)?;
    }
    let l = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(l)
}

/// Accept clients until the task is aborted.
pub async fn serve(listener: UnixListener, handle: DriverHandle) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let handle = handle.clone();
                tokio::spawn(async move {
                    if let Err(e) = client_task(stream, handle).await {
                        tracing::debug!("local client ended: {e}");
                    }
                });
            }
            Err(e) => {
                tracing::error!("local socket accept: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

async fn client_task(stream: UnixStream, handle: DriverHandle) -> std::io::Result<()> {
    let (mut rd, mut wr) = stream.into_split();
    let (client, mut events) = handle.attach();
    // Requests are read in their own task so a long socket write never splits a message.
    let (req_tx, mut req_rx) = tokio::sync::mpsc::channel::<std::io::Result<Option<Vec<u8>>>>(1);
    let reader = tokio::spawn(async move {
        loop {
            let m = read_raw(&mut rd).await;
            let stop = !matches!(m, Ok(Some(_)));
            if req_tx.send(m).await.is_err() || stop {
                return;
            }
        }
    });
    let result = client_loop(&mut wr, &handle, client, &mut events, &mut req_rx).await;
    reader.abort();
    handle.detach(client);
    result
}

enum Input {
    Event(Option<ClientMsg>),
    Request(Option<std::io::Result<Option<Vec<u8>>>>),
}

async fn client_loop(
    wr: &mut tokio::net::unix::OwnedWriteHalf,
    handle: &DriverHandle,
    client: u64,
    events: &mut tokio::sync::mpsc::Receiver<ClientMsg>,
    reqs: &mut tokio::sync::mpsc::Receiver<std::io::Result<Option<Vec<u8>>>>,
) -> std::io::Result<()> {
    use tokio::sync::mpsc::error::TryRecvError;
    // Subscriptions the core said are ready and that we haven't drained yet.
    let mut ready: BTreeSet<u32> = BTreeSet::new();
    loop {
        // Take what is available without waiting; otherwise drain one ready
        // subscription (only here, after the previous write completed); otherwise wait.
        let input = match events.try_recv() {
            Ok(m) => Input::Event(Some(m)),
            Err(TryRecvError::Disconnected) => Input::Event(None),
            Err(TryRecvError::Empty) => match reqs.try_recv() {
                Ok(r) => Input::Request(Some(r)),
                Err(TryRecvError::Disconnected) => Input::Request(None),
                Err(TryRecvError::Empty) => {
                    if let Some(&sub) = ready.iter().next() {
                        ready.remove(&sub);
                        let updates = handle.drain(client, sub, DRAIN_BATCH).await;
                        if updates.len() >= DRAIN_BATCH {
                            ready.insert(sub);
                        }
                        if !updates.is_empty() {
                            let ev = Event::Updates {
                                sub,
                                updates: updates.iter().map(update_json).collect(),
                            };
                            write_msg(wr, &ev).await?;
                        }
                        continue;
                    }
                    tokio::select! {
                        m = events.recv() => Input::Event(m),
                        r = reqs.recv() => Input::Request(r),
                    }
                }
            },
        };
        match input {
            // The driver detached us (we were too slow) or is gone.
            Input::Event(None) => return Ok(()),
            Input::Event(Some(msg)) => {
                if let LocalEvent::Ready { sub } = &msg.event {
                    ready.insert(*sub);
                } else if let Some(ev) = event_json(&msg) {
                    write_msg(wr, &ev).await?;
                }
            }
            Input::Request(None) | Input::Request(Some(Ok(None))) => return Ok(()),
            Input::Request(Some(Err(e))) => return Err(e),
            Input::Request(Some(Ok(Some(body)))) => {
                handle_request(&body, handle, client, wr).await?
            }
        }
    }
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

async fn handle_request(
    body: &[u8],
    handle: &DriverHandle,
    client: u64,
    wr: &mut tokio::net::unix::OwnedWriteHalf,
) -> std::io::Result<()> {
    let req: Request = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(e) => {
            return write_msg(
                wr,
                &Event::Error {
                    message: format!("bad request: {e}"),
                },
            )
            .await
        }
    };
    match req.to_command(unix_ms()) {
        Ok(Some(cmd)) => handle.local(client, cmd),
        Ok(None) => {
            if let Request::Drain { sub, max } = req {
                let updates = handle.drain(client, sub, max.clamp(1, 1024)).await;
                let ev = Event::Updates {
                    sub,
                    updates: updates.iter().map(update_json).collect(),
                };
                write_msg(wr, &ev).await?;
            }
        }
        Err(message) => write_msg(wr, &Event::Error { message }).await?,
    }
    Ok(())
}

// ---- client -----------------------------------------------------------------------

/// A blocking-style client for `prim ctl` and tests.
pub struct Client {
    stream: UnixStream,
    next_req: u64,
    /// Events that arrived while waiting for a reply.
    pub backlog: std::collections::VecDeque<Event>,
}

impl Client {
    pub async fn connect(path: &Path) -> std::io::Result<Client> {
        Ok(Client {
            stream: UnixStream::connect(path).await?,
            next_req: 1,
            backlog: Default::default(),
        })
    }

    pub fn next_req(&mut self) -> u64 {
        let r = self.next_req;
        self.next_req += 1;
        r
    }

    pub async fn send(&mut self, req: &Request) -> std::io::Result<()> {
        write_msg(&mut self.stream, req).await
    }

    /// Next event (from the backlog first). `Ok(None)` when the daemon closed the socket.
    pub async fn next_event(&mut self) -> std::io::Result<Option<Event>> {
        if let Some(e) = self.backlog.pop_front() {
            return Ok(Some(e));
        }
        self.read_event().await
    }

    async fn read_event(&mut self) -> std::io::Result<Option<Event>> {
        match read_raw(&mut self.stream).await? {
            None => Ok(None),
            Some(b) => serde_json::from_slice(&b)
                .map(Some)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        }
    }

    /// Send a request built from a fresh request id and wait for its reply. Other events
    /// are kept in `backlog`.
    pub async fn call(&mut self, make: impl FnOnce(u64) -> Request) -> std::io::Result<ReplyJson> {
        let req = self.next_req();
        self.send(&make(req)).await?;
        loop {
            match self.read_event().await? {
                None => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "daemon closed the socket",
                    ))
                }
                Some(Event::Reply { req: r, reply }) if r == req => return Ok(reply),
                Some(Event::Error { message }) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        message,
                    ))
                }
                Some(other) => self.backlog.push_back(other),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_map_to_commands() {
        let r: Request = serde_json::from_str(
            r#"{"op":"write","req":3,"scope":"member","name":"note","persistent":true,"value":"aGk="}"#,
        )
        .unwrap();
        match r.to_command(0).unwrap().unwrap() {
            LocalCommand::Write {
                req,
                scope,
                name,
                class,
                lifetime,
                value,
            } => {
                assert_eq!((req, scope, name.as_str()), (3, Scope::Member, "note"));
                assert_eq!((class, lifetime), (Class::Durable, Lifetime::Persistent));
                assert_eq!(value, b"hi");
            }
            other => panic!("{other:?}"),
        }
        let r: Request =
            serde_json::from_str(r#"{"op":"create_invite","req":1,"expiry_in_ms":1000}"#).unwrap();
        match r.to_command(5_000).unwrap().unwrap() {
            LocalCommand::CreateInvite { expiry_ms, .. } => assert_eq!(expiry_ms, 6_000),
            other => panic!("{other:?}"),
        }
        let r: Request = serde_json::from_str(
            r#"{"op":"send","req":1,"to":"coordinator","channel":2,"payload":""}"#,
        )
        .unwrap();
        assert!(matches!(
            r.to_command(0).unwrap().unwrap(),
            LocalCommand::Send {
                to: To::Coordinator,
                channel: 2,
                ..
            }
        ));
        let r: Request = serde_json::from_str(r#"{"op":"drain","sub":4}"#).unwrap();
        assert!(r.to_command(0).unwrap().is_none());
        let bad: Request =
            serde_json::from_str(r#"{"op":"revoke","req":1,"subject":"zz"}"#).unwrap();
        assert!(bad.to_command(0).is_err());
        assert!(serde_json::from_str::<Request>(r#"{"op":"nonsense"}"#).is_err());
    }

    #[test]
    fn updates_serialise_with_hex_keys_and_base64() {
        let u = Update::Value {
            path: "member/x/y".into(),
            writer: Key([0xab; 32]),
            version: room_core::staterec::Version {
                generation: 1,
                term: 2,
                seq: 3,
            },
            class: Class::Live,
            value: b"v".to_vec(),
        };
        let j = serde_json::to_value(update_json(&u)).unwrap();
        assert_eq!(j["kind"], "value");
        assert_eq!(j["writer"], "ab".repeat(32));
        assert_eq!(j["value"], "dg==");
        assert_eq!(j["class"], "live");
        assert_eq!(j["version"]["seq"], 3);
    }

    #[tokio::test]
    async fn framing_round_trips_and_caps() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        write_msg(&mut a, &Request::Status { req: 9 })
            .await
            .unwrap();
        let raw = read_raw(&mut b).await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_slice::<Request>(&raw).unwrap(),
            Request::Status { req: 9 }
        );
        drop(a);
        assert!(read_raw(&mut b).await.unwrap().is_none());
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&((MAX_MESSAGE as u32 + 1).to_le_bytes()))
            .await
            .unwrap();
        assert!(read_raw(&mut b).await.is_err());
    }
}
