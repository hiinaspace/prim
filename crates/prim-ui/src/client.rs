//! The client layer the window, `prim ctl chat` and the headless tests all use: one
//! connection to the daemon's `room.sock` that keeps a [`Model`] of the live room
//! (status, every state key, chat) and takes [`Cmd`]s.
//!
//! The model is plain data behind a mutex. The window reads it every frame, the tests
//! poll it. Nothing here knows about egui.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use prim_room::local::{
    b64, read_raw, unb64, write_msg, Event, ReplyJson, Request, StatusJson, UpdateJson,
    VersionJson,
};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use crate::chat::{self, Clients, CHAT_CHANNEL};

const SUB_ALL: u32 = 1;
const MAX_CHAT_LINES: usize = 1000;

#[derive(Clone, Debug)]
pub struct KeyEntry {
    pub writer: String,
    pub version: VersionJson,
    pub class: String,
    pub value: Vec<u8>,
    /// When this daemon last saw the key change (the record carries no wall time).
    pub changed: Instant,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Delivery {
    Pending,
    /// Delivered to `ok` of `total` other members; `failed` lists `(member, status)`.
    Done {
        ok: u32,
        total: u32,
        failed: Vec<(String, String)>,
    },
    /// The daemon refused the send.
    Error(String),
}

#[derive(Clone, Debug)]
pub struct ChatLine {
    pub id: u64,
    pub at: SystemTime,
    /// Member key (hex); `None` is a system line ("alex joined").
    pub from: Option<String>,
    pub text: String,
    /// Only on lines this connection sent.
    pub delivery: Option<Delivery>,
}

#[derive(Default)]
pub struct Model {
    /// False once the daemon closed the socket.
    pub attached: bool,
    pub status: Option<StatusJson>,
    pub keys: BTreeMap<String, KeyEntry>,
    pub snapshot_done: bool,
    pub chat: Vec<ChatLine>,
    /// Newest minted invite, with a counter so the window can tell a new one.
    pub invite: Option<(u64, String)>,
    /// Newest command error, for the status bar.
    pub notice: Option<String>,
    /// The daemon said this node was removed from the room.
    pub removed: bool,
    next_line: u64,
    invite_seq: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    Online,
    /// Online in the roster but its presence is stale.
    NotResponding,
    Offline,
}

#[derive(Clone, Debug)]
pub struct Route {
    pub path: String,
    pub rtt_ms: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct MemberRow {
    pub key: String,
    pub name: String,
    pub is_me: bool,
    pub presence: Presence,
    pub keyholder: bool,
    pub coordinator: bool,
    pub route: Option<Route>,
    /// `Some((listening, mic_on))` from `member/<id>/voice`.
    pub voice: Option<(bool, bool)>,
    pub clients: Clients,
}

impl Model {
    pub fn me(&self) -> Option<&str> {
        self.status.as_ref().map(|s| s.me.as_str())
    }

    pub fn in_room(&self) -> bool {
        self.status
            .as_ref()
            .is_some_and(|s| s.room.is_some() && s.lifecycle != "dead")
    }

    fn key_value(&self, path: &str) -> Option<&[u8]> {
        self.keys.get(path).map(|k| k.value.as_slice())
    }

    pub fn name_of(&self, key: &str) -> String {
        self.key_value(&format!("member/{key}/profile"))
            .and_then(chat::decode_profile)
            .unwrap_or_else(|| short(key))
    }

    pub fn members(&self) -> Vec<MemberRow> {
        let Some(st) = &self.status else {
            return vec![];
        };
        let coord = st.coordinator.as_ref().map(|c| c.holder.as_str());
        let mut rows: Vec<MemberRow> = st
            .members
            .iter()
            .map(|m| {
                let is_me = m.key == st.me;
                let presence = if is_me {
                    Presence::Online
                } else if !m.online {
                    Presence::Offline
                } else if m.fresh {
                    Presence::Online
                } else {
                    Presence::NotResponding
                };
                let route = st.routes.iter().find(|r| r.peer == m.key).map(|r| Route {
                    path: r.path.clone(),
                    rtt_ms: r.rtt_ms,
                });
                MemberRow {
                    name: self.name_of(&m.key),
                    is_me,
                    presence,
                    keyholder: m.keyholder,
                    coordinator: coord == Some(m.key.as_str()),
                    route,
                    voice: self
                        .key_value(&format!("member/{}/voice", m.key))
                        .and_then(chat::decode_voice)
                        .filter(|_| presence != Presence::Offline),
                    clients: self
                        .key_value(&format!("member/{}/clients", m.key))
                        .and_then(chat::decode_clients)
                        .unwrap_or_default(),
                    key: m.key.clone(),
                }
            })
            .collect();
        // You first, then online before offline, then by name.
        rows.sort_by(|a, b| {
            (!a.is_me, a.presence == Presence::Offline, a.name.to_lowercase()).cmp(&(
                !b.is_me,
                b.presence == Presence::Offline,
                b.name.to_lowercase(),
            ))
        });
        rows
    }

    /// True when the daemon holds a `voice` lease (any program).
    pub fn voice_lease_held(&self) -> bool {
        self.status
            .as_ref()
            .and_then(|s| s.rt.as_ref())
            .is_some_and(|rt| rt.holders.iter().any(|h| h.role == "voice"))
    }

    fn push_line(&mut self, from: Option<String>, text: String, delivery: Option<Delivery>) -> u64 {
        let id = self.next_line;
        self.next_line += 1;
        self.chat.push(ChatLine {
            id,
            at: SystemTime::now(),
            from,
            text,
            delivery,
        });
        if self.chat.len() > MAX_CHAT_LINES {
            self.chat.remove(0);
        }
        id
    }
}

pub fn short(key: &str) -> String {
    key.chars().take(8).collect()
}

/// What the window (or a script) asks the connection to do.
pub enum Cmd {
    Chat(String),
    SetName(String),
    SetClients(Clients),
    Invite,
    Leave,
    Close,
    Status,
}

#[derive(Clone)]
pub struct Handle {
    tx: mpsc::UnboundedSender<Cmd>,
    pub model: Arc<Mutex<Model>>,
}

impl Handle {
    pub fn send(&self, c: Cmd) {
        let _ = self.tx.send(c);
    }
    pub fn chat(&self, text: &str) {
        self.send(Cmd::Chat(text.to_string()));
    }
    pub fn set_name(&self, name: &str) {
        self.send(Cmd::SetName(name.to_string()));
    }
    pub fn lock(&self) -> std::sync::MutexGuard<'_, Model> {
        self.model.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// Poll the model until `f` holds (tests and scripts).
    pub async fn wait_for(
        &self,
        secs: u64,
        what: &str,
        mut f: impl FnMut(&Model) -> bool,
    ) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            if f(&self.lock()) {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(format!("timed out waiting for {what}"));
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    }
}

impl Handle {
    /// Like `wait_for`, but the predicate runs without the model lock held, so it may
    /// call `lock()` itself (helpers that take a `Handle`).
    pub async fn wait_until(&self, secs: u64, what: &str, mut f: impl FnMut() -> bool) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            if f() {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(format!("timed out waiting for {what}"));
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    }
}

#[derive(Clone, Default)]
pub struct AttachOpts {
    /// Called after every model change (the window requests a repaint).
    pub repaint: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Written as `member/<me>/profile` if the member has none yet.
    pub default_name: Option<String>,
    /// Register the chat channel (everything except one-shot senders).
    pub chat: bool,
}

enum Follow {
    None,
    Status,
    Name(String),
}

enum Pending {
    Plain(&'static str),
    Status,
    Invite,
    /// A chat line's send; `id` is the line.
    Chat(u64),
}

/// Connect to the daemon at `socket` and start the model task.
pub async fn attach(socket: &Path, opts: AttachOpts) -> std::io::Result<Handle> {
    let stream = UnixStream::connect(socket).await?;
    let (mut rd, mut wr) = stream.into_split();
    let model = Arc::new(Mutex::new(Model {
        attached: true,
        ..Default::default()
    }));
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<Option<Event>>();
    tokio::spawn(async move {
        loop {
            match read_raw(&mut rd).await {
                Ok(Some(b)) => match serde_json::from_slice::<Event>(&b) {
                    Ok(ev) => {
                        if ev_tx.send(Some(ev)).is_err() {
                            return;
                        }
                    }
                    Err(_) => continue,
                },
                _ => {
                    let _ = ev_tx.send(None);
                    return;
                }
            }
        }
    });
    let (tx, mut rx) = mpsc::unbounded_channel::<Cmd>();
    let handle = Handle {
        tx,
        model: model.clone(),
    };
    tokio::spawn(async move {
        let mut next_req = 1u64;
        let mut pending: BTreeMap<u64, Pending> = BTreeMap::new();
        // msg_id of a chat line's All-send -> line id.
        let mut sends: BTreeMap<u64, u64> = BTreeMap::new();
        let mut tick = tokio::time::interval(Duration::from_millis(1000));
        let repaint = |o: &AttachOpts| {
            if let Some(f) = &o.repaint {
                f();
            }
        };
        'run: {
        macro_rules! req {
            ($p:expr, $make:expr) => {{
                let r = next_req;
                next_req += 1;
                pending.insert(r, $p);
                #[allow(clippy::redundant_closure_call)]
                let rq: Request = ($make)(r);
                if write_msg(&mut wr, &rq).await.is_err() {
                    break 'run;
                }
            }};
        }
        if opts.chat {
            req!(Pending::Plain("register chat"), |req| {
                Request::RegisterChannel {
                    req,
                    channel: CHAT_CHANNEL,
                }
            });
        }
        req!(Pending::Plain("subscribe"), |req| Request::Subscribe {
            req,
            sub: SUB_ALL,
            pattern: "**".into()
        });
        req!(Pending::Status, |req| Request::Status { req });
        loop {
            tokio::select! {
                ev = ev_rx.recv() => {
                    let Some(Some(ev)) = ev else { break };
                    let follow = { let mut m = model.lock().unwrap_or_else(|e| e.into_inner());
                    let mut follow = Follow::None;
                    match ev {
                        Event::Reply { req, reply } => match (pending.remove(&req), reply) {
                            (Some(Pending::Status), ReplyJson::Status { status }) => {
                                let first = m.status.is_none();
                                m.status = Some(status);
                                let _ = first;
                            }
                            (Some(Pending::Invite), ReplyJson::Invite { token }) => {
                                m.invite_seq += 1;
                                let seq = m.invite_seq;
                                m.invite = Some((seq, token));
                            }
                            (Some(Pending::Chat(id)), ReplyJson::Sending { msg_id, targets }) => {
                                sends.insert(msg_id, id);
                                let total = targets.len() as u32;
                                if let Some(l) = m.chat.iter_mut().find(|l| l.id == id) {
                                    l.delivery = Some(Delivery::Done { ok: 0, total, failed: vec![] });
                                }
                            }
                            (Some(Pending::Chat(id)), ReplyJson::Error { code }) => {
                                if let Some(l) = m.chat.iter_mut().find(|l| l.id == id) {
                                    l.delivery = Some(Delivery::Error(code));
                                }
                            }
                            (Some(Pending::Plain(what)), ReplyJson::Error { code }) => {
                                m.notice = Some(format!("{what}: {code}"));
                            }
                            (_, ReplyJson::Error { code }) => m.notice = Some(code),
                            _ => {}
                        },
                        Event::Updates { updates, .. } => {
                            for u in updates {
                                match u {
                                    UpdateJson::Reset => {
                                        m.keys.clear();
                                        m.snapshot_done = false;
                                    }
                                    UpdateJson::Value { path, writer, version, class, value } => {
                                        let value = unb64(&value).unwrap_or_default();
                                        m.keys.insert(path, KeyEntry { writer, version, class, value, changed: Instant::now() });
                                    }
                                    UpdateJson::Removed { path } => {
                                        m.keys.remove(&path);
                                    }
                                    UpdateJson::SnapshotDone => m.snapshot_done = true,
                                }
                            }
                            // Give the member a name the first time we see it has none.
                            let me = m.me().map(str::to_string);
                            if let (true, Some(me), Some(name)) = (m.snapshot_done, me, &opts.default_name) {
                                if !m.keys.contains_key(&format!("member/{me}/profile")) {
                                    // Pretend it is set, so we write once; the real value overwrites.
                                    m.keys.insert(format!("member/{me}/profile"), KeyEntry {
                                        writer: me.clone(), version: VersionJson { generation: 0, term: 0, seq: 0 },
                                        class: "durable".into(), value: chat::encode_profile(name), changed: Instant::now(),
                                    });
                                    follow = Follow::Name(b64(&chat::encode_profile(name)));
                                }
                            }
                        }
                        Event::Message { from, channel, payload } if channel == CHAT_CHANNEL => {
                            if let Some(text) = unb64(&payload).ok().and_then(|p| chat::decode_chat(&p)) {
                                m.push_line(Some(from), text, None);
                            }
                        }
                        Event::MessageResult { msg_id, to, status } => {
                            if let Some(&line) = sends.get(&msg_id) {
                                if let Some(l) = m.chat.iter_mut().find(|l| l.id == line) {
                                    if let Some(Delivery::Done { ok, failed, .. }) = &mut l.delivery {
                                        if status == "delivered" { *ok += 1 } else { failed.push((to, status)) }
                                    }
                                }
                            }
                        }
                        Event::Member { key, what } => {
                            if m.me() != Some(key.as_str()) {
                                let name = m.name_of(&key);
                                // Before a member's profile arrives there is only a key to show.
                                let named = name != short(&key);
                                let line = match what.as_str() {
                                    "joined" => Some(format!("{name} joined the room")),
                                    "online" if named => Some(format!("{name} is online")),
                                    "offline" => Some(format!("{name} went offline")),
                                    "unresponsive" => Some(format!("{name} stopped responding")),
                                    "removed" => Some(format!("{name} was removed")),
                                    _ => None,
                                };
                                if let Some(t) = line { m.push_line(None, t, None); }
                            }
                            follow = Follow::Status;
                        }
                        Event::RoomClosed { .. } => {
                            m.push_line(None, "The room was closed by its opener".into(), None);
                            follow = Follow::Status;
                        }
                        Event::RoomOpened { .. } => {}
                        Event::Removed => m.removed = true,
                        Event::Error { message } => m.notice = Some(message),
                        _ => {}
                    }
                    follow };
                    match follow {
                        Follow::None => {}
                        Follow::Status => req!(Pending::Status, |req| Request::Status { req }),
                        Follow::Name(value) => req!(Pending::Plain("set name"), |req| Request::Write {
                            req, scope: "member".into(), name: "profile".into(), live: false, persistent: true, value: value.clone() }),
                    }
                    repaint(&opts);
                }
                cmd = rx.recv() => {
                    let Some(cmd) = cmd else { break };
                    match cmd {
                        Cmd::Chat(text) => {
                            let text = text.trim().to_string();
                            if text.is_empty() { continue; }
                            let id = {
                                let mut m = model.lock().unwrap_or_else(|e| e.into_inner());
                                let me = m.me().map(str::to_string);
                                m.push_line(me, text.clone(), Some(Delivery::Pending))
                            };
                            let payload = b64(&chat::encode_chat(&text));
                            req!(Pending::Chat(id), |req| Request::Send {
                                req, to: "all".into(), channel: CHAT_CHANNEL, reliable: true, payload: payload.clone() });
                        }
                        Cmd::SetName(name) => {
                            let name = name.trim().to_string();
                            if name.is_empty() { continue; }
                            let value = b64(&chat::encode_profile(&name));
                            req!(Pending::Plain("set name"), |req| Request::Write {
                                req, scope: "member".into(), name: "profile".into(), live: false, persistent: true, value: value.clone() });
                        }
                        Cmd::SetClients(c) => {
                            let value = b64(&chat::encode_clients(c));
                            req!(Pending::Plain("update clients"), |req| Request::Write {
                                req, scope: "member".into(), name: "clients".into(), live: false, persistent: false, value: value.clone() });
                        }
                        Cmd::Invite => req!(Pending::Invite, |req| Request::CreateInvite { req, expiry_in_ms: 3_600_000, relays: vec![] }),
                        Cmd::Leave => req!(Pending::Plain("leave"), |req| Request::Leave { req }),
                        Cmd::Close => req!(Pending::Plain("close room"), |req| Request::Close { req }),
                        Cmd::Status => req!(Pending::Status, |req| Request::Status { req }),
                    }
                    repaint(&opts);
                }
                _ = tick.tick() => {
                    req!(Pending::Status, |req| Request::Status { req });
                    repaint(&opts);
                }
            }
        }
        }
        model.lock().unwrap_or_else(|e| e.into_inner()).attached = false;
        repaint(&opts);
    });
    Ok(handle)
}

/// One-shot: post a chat line from a script (`prim ctl chat send`). Sends to every
/// other member and to this daemon's own local clients, so a window on the same
/// machine shows it too. Returns `(delivered, total, failures)` over the other members.
pub async fn send_chat_once(
    socket: &Path,
    text: &str,
    timeout: Duration,
) -> std::io::Result<(u32, u32, Vec<(String, String)>)> {
    let mut c = prim_room::local::Client::connect(socket).await?;
    let payload = b64(&chat::encode_chat(text));
    let me = match c.call(|req| Request::Status { req }).await? {
        ReplyJson::Status { status } => status.me,
        other => return Err(std::io::Error::other(format!("status: {other:?}"))),
    };
    // Local copy first: it needs no network.
    let _ = c
        .call(|req| Request::Send {
            req,
            to: me.clone(),
            channel: CHAT_CHANNEL,
            reliable: true,
            payload: payload.clone(),
        })
        .await?;
    let (msg_id, targets) = match c
        .call(|req| Request::Send {
            req,
            to: "all".into(),
            channel: CHAT_CHANNEL,
            reliable: true,
            payload: payload.clone(),
        })
        .await?
    {
        ReplyJson::Sending { msg_id, targets } => (msg_id, targets),
        ReplyJson::Error { code } => return Err(std::io::Error::other(code)),
        other => return Err(std::io::Error::other(format!("send: {other:?}"))),
    };
    let total = targets.len() as u32;
    let (mut ok, mut failed) = (0u32, vec![]);
    let _ = tokio::time::timeout(timeout, async {
        while ok + (failed.len() as u32) < total {
            match c.next_event().await {
                Ok(Some(Event::MessageResult { msg_id: id, to, status })) if id == msg_id => {
                    if status == "delivered" {
                        ok += 1
                    } else {
                        failed.push((to, status))
                    }
                }
                Ok(Some(_)) => {}
                _ => break,
            }
        }
    })
    .await;
    Ok((ok, total, failed))
}
