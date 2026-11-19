//! The room model: what every Prim client needs to know about the room it is attached
//! to, kept from the daemon's events (status, every state key, chat), plus the commands
//! that change it. Moved here from `prim_ui::client` in round A1 so Godot (through
//! `PrimClient`), `prim ctl`, the tests and the dev window share one implementation.
//!
//! [`Room`] owns a [`Client`] and is single-threaded: its owner calls [`Room::pump`] on
//! its own cadence (Godot's `_process`, a script loop) and reads [`Room::model`].
//! [`Handle`] wraps a `Room` in a mutex with a small pump thread, for owners that want
//! a shared model instead (the dev window, tests).
//!
//! Nothing here knows about egui, Godot or the supervisor.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;

use crate::chat::{self, Clients, CHAT_CHANNEL};
use crate::client::{Client, ClientEvent};
use crate::transport::Transport;
use crate::wire::{b64, unb64, Event, ReplyJson, Request, StatusJson, UpdateJson, VersionJson};

const MAX_CHAT_LINES: usize = 1000;
const STATUS_EVERY: Duration = Duration::from_millis(1000);

#[derive(Clone, Debug)]
pub struct KeyEntry {
    pub writer: String,
    pub version: VersionJson,
    pub class: String,
    pub value: Vec<u8>,
    /// When this process last saw the key change (the record carries no wall time).
    pub changed: Instant,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
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
    /// False while the daemon socket is down.
    pub attached: bool,
    pub status: Option<StatusJson>,
    pub keys: BTreeMap<String, KeyEntry>,
    pub snapshot_done: bool,
    pub chat: Vec<ChatLine>,
    /// Newest minted invite, with a counter so a UI can tell a new one.
    pub invite: Option<(u64, String)>,
    /// Newest command error, for a status bar.
    pub notice: Option<String>,
    /// The daemon said this node was removed from the room.
    pub removed: bool,
    next_line: u64,
    invite_seq: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
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

    /// The room's name (`found --meta`), if it has a readable one.
    pub fn room_name(&self) -> Option<&str> {
        self.status.as_ref().and_then(|s| s.name.as_deref())
    }

    /// "none", "pending" or "refused" (plus why in [`Model::join_refused`]).
    pub fn join(&self) -> &str {
        self.status.as_ref().map_or("none", |s| s.join.as_str())
    }

    pub fn join_refused(&self) -> Option<&str> {
        self.status.as_ref().and_then(|s| s.join_refused.as_deref())
    }

    pub fn key_value(&self, path: &str) -> Option<&[u8]> {
        self.keys.get(path).map(|k| k.value.as_slice())
    }

    pub fn name_of(&self, key: &str) -> String {
        self.key_value(&format!("member/{key}/profile"))
            .and_then(chat::decode_profile)
            .unwrap_or_else(|| short(key))
    }

    /// My display name if I have written one.
    pub fn my_name(&self) -> Option<String> {
        let me = self.me()?;
        self.key_value(&format!("member/{me}/profile"))
            .and_then(chat::decode_profile)
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
        self.voice_holder().is_some()
    }

    /// `(session, incarnation)` of the program holding the `voice` lease: it changes
    /// whenever voice is replaced, so tests and tools can tell "same voice" from "new".
    pub fn voice_holder(&self) -> Option<(u64, u32)> {
        self.status
            .as_ref()
            .and_then(|s| s.rt.as_ref())
            .and_then(|rt| rt.holders.iter().find(|h| h.role == "voice"))
            .map(|h| (h.session, h.incarnation))
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

    /// A plain, serializable picture of the room for scripts and Godot: everything a menu
    /// shows. Keys are hex; times are not included.
    pub fn view(&self) -> serde_json::Value {
        let members: Vec<_> = self
            .members()
            .into_iter()
            .map(|m| {
                serde_json::json!({
                    "key": m.key, "name": m.name, "is_me": m.is_me,
                    "presence": m.presence, "coordinator": m.coordinator,
                    "keyholder": m.keyholder,
                    "path": m.route.as_ref().map(|r| r.path.clone()),
                    "rtt_ms": m.route.as_ref().and_then(|r| r.rtt_ms),
                    "listening": m.voice.map(|v| v.0), "mic": m.voice.map(|v| v.1),
                    "deaf": m.clients.deaf, "godot": m.clients.godot, "player": m.clients.player,
                })
            })
            .collect();
        let holder = self.voice_holder();
        serde_json::json!({
            "attached": self.attached,
            "in_room": self.in_room(),
            "lifecycle": self.status.as_ref().map(|s| s.lifecycle.clone()),
            "room": self.status.as_ref().and_then(|s| s.room.clone()),
            "room_name": self.room_name(),
            "me": self.me(),
            "my_name": self.my_name(),
            "join": self.join(),
            "join_refused": self.join_refused(),
            "removed": self.removed,
            "notice": self.notice,
            "invite": self.invite.as_ref().map(|i| i.1.clone()),
            "voice_session": holder.map(|h| h.0),
            "voice_incarnation": holder.map(|h| h.1),
            "members": members,
        })
    }
}

pub fn short(key: &str) -> String {
    key.chars().take(8).collect()
}

/// What an owner asks the room to do.
pub enum Cmd {
    Chat(String),
    SetName(String),
    SetClients(Clients),
    Invite,
    Revoke(String),
    Leave,
    Close,
    Status,
}

enum Pending {
    Plain(&'static str),
    Status,
    Invite,
    /// A chat line's send; `id` is the line.
    Chat(u64),
}

#[derive(Clone, Default)]
pub struct RoomOpts {
    /// Called after every pump that changed the model (the dev window requests a repaint).
    pub repaint: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Written as `member/<me>/profile` if the member has none yet.
    pub default_name: Option<String>,
    /// Register the chat channel (everything except one-shot senders).
    pub chat: bool,
}

pub struct Room {
    client: Client,
    pub model: Model,
    opts: RoomOpts,
    sub_all: u32,
    pending: BTreeMap<u64, Pending>,
    /// msg_id of a chat line's All-send -> line id.
    sends: BTreeMap<u64, u64>,
    last_status: Instant,
    /// The first `Connected` has not been seen yet.
    first: bool,
}

impl Room {
    /// Connect (in the background) and subscribe to everything. Never blocks; the model
    /// fills as [`Room::pump`] runs.
    pub fn start(transport: impl Transport, opts: RoomOpts) -> Room {
        let client = Client::start(transport);
        let sub_all = client.subscribe("**").unwrap_or(0);
        if opts.chat {
            let _ = client.register_channel(CHAT_CHANNEL);
        }
        Room {
            client,
            model: Model::default(),
            opts,
            sub_all,
            pending: BTreeMap::new(),
            sends: BTreeMap::new(),
            last_status: Instant::now(),
            first: true,
        }
    }

    /// True for the subscription the model made for itself (not the caller's).
    pub fn owns_sub(&self, sub: u32) -> bool {
        sub == self.sub_all
    }

    /// The underlying client, for the raw operations Godot also exposes (state writes,
    /// datagrams, other subscriptions). Events it produces come back out of `pump`.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Take every waiting event, fold it into the model, send any follow-up requests, and
    /// hand the events back so the owner can also react to the raw stream. Returns an
    /// empty list when nothing happened.
    pub fn pump(&mut self) -> Vec<ClientEvent> {
        let mut out = vec![];
        while let Some(ev) = self.client.try_recv() {
            self.apply(&ev);
            out.push(ev);
        }
        if self.client.is_connected() && self.last_status.elapsed() >= STATUS_EVERY {
            self.last_status = Instant::now();
            self.request_status();
        }
        if !out.is_empty() {
            if let Some(f) = &self.opts.repaint {
                f();
            }
        }
        out
    }

    fn request_status(&mut self) {
        if let Ok(r) = self.client.request(|req| Request::Status { req }) {
            self.pending.insert(r, Pending::Status);
        }
    }

    fn track(&mut self, p: Pending, r: std::io::Result<u64>) {
        match r {
            Ok(id) => {
                self.pending.insert(id, p);
            }
            Err(e) => self.model.notice = Some(e.to_string()),
        }
    }

    fn apply(&mut self, ev: &ClientEvent) {
        match ev {
            ClientEvent::Connected => {
                self.model.attached = true;
                self.first = false;
                self.request_status();
            }
            ClientEvent::Disconnected => {
                self.model.attached = false;
                // Replies in flight are lost with the connection.
                self.pending.clear();
            }
            ClientEvent::Unknown(_) => {}
            ClientEvent::Event(e) => self.apply_event(e),
        }
    }

    fn apply_event(&mut self, ev: &Event) {
        let m = &mut self.model;
        match ev {
            Event::Reply { req, reply } => match (self.pending.remove(req), reply) {
                (Some(Pending::Status), ReplyJson::Status { status }) => {
                    if let Some(st) = StatusJson::from_value(status) {
                        m.status = Some(st);
                    }
                }
                (Some(Pending::Invite), ReplyJson::Invite { token }) => {
                    m.invite_seq += 1;
                    m.invite = Some((m.invite_seq, token.clone()));
                }
                (Some(Pending::Chat(id)), ReplyJson::Sending { msg_id, targets }) => {
                    self.sends.insert(*msg_id, id);
                    let total = targets.len() as u32;
                    if let Some(l) = m.chat.iter_mut().find(|l| l.id == id) {
                        l.delivery = Some(Delivery::Done {
                            ok: 0,
                            total,
                            failed: vec![],
                        });
                    }
                }
                (Some(Pending::Chat(id)), ReplyJson::Error { code }) => {
                    if let Some(l) = m.chat.iter_mut().find(|l| l.id == id) {
                        l.delivery = Some(Delivery::Error(code.clone()));
                    }
                }
                (Some(Pending::Plain(what)), ReplyJson::Error { code }) => {
                    m.notice = Some(format!("{what}: {code}"));
                }
                (_, ReplyJson::Error { code }) => m.notice = Some(code.clone()),
                _ => {}
            },
            Event::Updates { sub, updates } if *sub == self.sub_all => {
                for u in updates {
                    match u {
                        UpdateJson::Reset => {
                            m.keys.clear();
                            m.snapshot_done = false;
                        }
                        UpdateJson::Value {
                            path,
                            writer,
                            version,
                            class,
                            value,
                        } => {
                            let value = unb64(value).unwrap_or_default();
                            m.keys.insert(
                                path.clone(),
                                KeyEntry {
                                    writer: writer.clone(),
                                    version: version.clone(),
                                    class: class.clone(),
                                    value,
                                    changed: Instant::now(),
                                },
                            );
                        }
                        UpdateJson::Removed { path } => {
                            m.keys.remove(path);
                        }
                        UpdateJson::SnapshotDone => m.snapshot_done = true,
                    }
                }
                // Give the member a name the first time we see it has none.
                let me = m.me().map(str::to_string);
                if let (true, Some(me), Some(name)) = (m.snapshot_done, me, self.opts.default_name.clone()) {
                    if !m.keys.contains_key(&format!("member/{me}/profile")) {
                        // Pretend it is set, so we write once; the real value overwrites.
                        m.keys.insert(
                            format!("member/{me}/profile"),
                            KeyEntry {
                                writer: me.clone(),
                                version: VersionJson {
                                    generation: 0,
                                    term: 0,
                                    seq: 0,
                                },
                                class: "durable".into(),
                                value: chat::encode_profile(&name),
                                changed: Instant::now(),
                            },
                        );
                        self.write_profile(&name);
                    }
                }
            }
            Event::Message {
                from,
                channel,
                payload,
            } if *channel == CHAT_CHANNEL && self.opts.chat => {
                if let Some(text) = unb64(payload).ok().and_then(|p| chat::decode_chat(&p)) {
                    m.push_line(Some(from.clone()), text, None);
                }
            }
            Event::MessageResult { msg_id, to, status } => {
                if let Some(&line) = self.sends.get(msg_id) {
                    if let Some(l) = m.chat.iter_mut().find(|l| l.id == line) {
                        if let Some(Delivery::Done { ok, failed, .. }) = &mut l.delivery {
                            if status == "delivered" {
                                *ok += 1
                            } else {
                                failed.push((to.clone(), status.clone()))
                            }
                        }
                    }
                }
            }
            Event::Member { key, what } => {
                if m.me() != Some(key.as_str()) {
                    let name = m.name_of(key);
                    // Before a member's profile arrives there is only a key to show.
                    let named = name != short(key);
                    let line = match what.as_str() {
                        "joined" if named => Some(format!("{name} joined the room")),
                        "online" if named => Some(format!("{name} is online")),
                        "offline" => Some(format!("{name} went offline")),
                        "unresponsive" => Some(format!("{name} stopped responding")),
                        "removed" => Some(format!("{name} was removed")),
                        _ => None,
                    };
                    if let Some(t) = line {
                        m.push_line(None, t, None);
                    }
                }
                self.request_status();
            }
            Event::RoomClosed { .. } => {
                m.push_line(None, "The room was closed by its opener".into(), None);
                self.request_status();
            }
            Event::Removed => m.removed = true,
            Event::Error { message } => m.notice = Some(message.clone()),
            _ => {}
        }
    }

    fn write_profile(&mut self, name: &str) {
        let value = chat::encode_profile(name);
        let r = self.client.write("member", "profile", false, true, &value);
        self.track(Pending::Plain("set name"), r);
    }

    // ---- commands -------------------------------------------------------------------

    pub fn send(&mut self, c: Cmd) {
        match c {
            Cmd::Chat(t) => self.chat(&t),
            Cmd::SetName(n) => self.set_name(&n),
            Cmd::SetClients(c) => self.set_clients(c),
            Cmd::Invite => self.invite(),
            Cmd::Revoke(k) => self.revoke(&k),
            Cmd::Leave => self.leave(),
            Cmd::Close => self.close_room(),
            Cmd::Status => self.request_status(),
        }
    }

    /// Post a chat line to the room (and keep it in this model's log).
    pub fn chat(&mut self, text: &str) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        let me = self.model.me().map(str::to_string);
        let id = self
            .model
            .push_line(me, text.clone(), Some(Delivery::Pending));
        let r = self
            .client
            .send("all", CHAT_CHANNEL, true, &chat::encode_chat(&text));
        self.track(Pending::Chat(id), r);
    }

    pub fn set_name(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        self.write_profile(name);
    }

    pub fn set_clients(&mut self, c: Clients) {
        let r = self
            .client
            .write("member", "clients", false, false, &chat::encode_clients(c));
        self.track(Pending::Plain("update clients"), r);
    }

    pub fn invite(&mut self) {
        self.invite_for(3_600_000);
    }

    /// An invite that lapses after `expiry_ms` (0 = the daemon's default).
    pub fn invite_for(&mut self, expiry_ms: u64) {
        let r = self.client.request(|req| Request::CreateInvite {
            req,
            expiry_in_ms: expiry_ms,
            relays: vec![],
        });
        self.track(Pending::Invite, r);
    }

    pub fn revoke(&mut self, key: &str) {
        let key = key.to_string();
        let r = self.client.request(|req| Request::Revoke { req, subject: key });
        self.track(Pending::Plain("revoke"), r);
    }

    pub fn leave(&mut self) {
        let r = self.client.request(|req| Request::Leave { req });
        self.track(Pending::Plain("leave"), r);
    }

    pub fn close_room(&mut self) {
        let r = self.client.request(|req| Request::Close { req });
        self.track(Pending::Plain("close room"), r);
    }

    pub fn close(&mut self) {
        self.client.close();
    }
}

/// A [`Room`] behind a mutex with a pump thread: the shared-model shape the dev window
/// and the tests use. Clone freely; the thread stops when the last clone drops.
#[derive(Clone)]
pub struct Handle {
    room: Arc<Mutex<Room>>,
    _stop: Arc<StopOnDrop>,
}

struct StopOnDrop(std::sync::atomic::AtomicBool);
impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Read access to the model of a [`Handle`]; derefs to [`Model`].
pub struct ModelGuard<'a>(MutexGuard<'a, Room>);
impl std::ops::Deref for ModelGuard<'_> {
    type Target = Model;
    fn deref(&self) -> &Model {
        &self.0.model
    }
}

impl Handle {
    pub fn attach(transport: impl Transport, opts: RoomOpts) -> Handle {
        let room = Arc::new(Mutex::new(Room::start(transport, opts)));
        let stop = Arc::new(StopOnDrop(std::sync::atomic::AtomicBool::new(false)));
        let (r2, s2) = (Arc::downgrade(&room), stop.clone());
        std::thread::Builder::new()
            .name("prim-room-pump".into())
            .spawn(move || {
                while !s2.0.load(std::sync::atomic::Ordering::Relaxed) {
                    let Some(r) = r2.upgrade() else { return };
                    r.lock().unwrap_or_else(|e| e.into_inner()).pump();
                    drop(r);
                    std::thread::sleep(Duration::from_millis(20));
                }
            })
            .expect("spawn pump thread");
        Handle { room, _stop: stop }
    }

    pub fn lock(&self) -> ModelGuard<'_> {
        ModelGuard(self.room.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn send(&self, c: Cmd) {
        self.room.lock().unwrap_or_else(|e| e.into_inner()).send(c);
    }
    pub fn chat(&self, text: &str) {
        self.send(Cmd::Chat(text.to_string()));
    }
    pub fn set_name(&self, name: &str) {
        self.send(Cmd::SetName(name.to_string()));
    }

    /// Poll the model until `f` holds (tests and scripts); blocking.
    pub fn wait_for(&self, secs: u64, what: &str, mut f: impl FnMut(&Model) -> bool) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            if f(&self.lock()) {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(format!("timed out waiting for {what}"));
            }
            std::thread::sleep(Duration::from_millis(40));
        }
    }

    /// Like `wait_for`, but the predicate runs without the model lock held, so it may
    /// call `lock()` itself (helpers that take a `Handle`).
    pub fn wait_until(&self, secs: u64, what: &str, mut f: impl FnMut() -> bool) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            if f() {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(format!("timed out waiting for {what}"));
            }
            std::thread::sleep(Duration::from_millis(40));
        }
    }
}

/// One-shot: post a chat line from a script (`prim ctl chat send`). Sends to every
/// other member and to this daemon's own local clients, so other programs on the same
/// machine show it too. Returns `(delivered, total, failures)` over the other members.
pub fn send_chat_once(
    transport: impl Transport,
    text: &str,
    timeout: Duration,
) -> std::io::Result<(u32, u32, Vec<(String, String)>)> {
    let client = Client::start(transport);
    let deadline = Instant::now() + timeout.max(Duration::from_secs(2));
    while !client.is_connected() {
        if Instant::now() > deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "no room daemon answering",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let payload = chat::encode_chat(text);
    let call = |make: &dyn Fn(u64) -> Request| client.call(make, Duration::from_secs(5));
    let me = match call(&|req| Request::Status { req })? {
        ReplyJson::Status { status } => status["me"].as_str().unwrap_or("").to_string(),
        other => return Err(std::io::Error::other(format!("status: {other:?}"))),
    };
    // Local copy first: it needs no network.
    let b = b64(&payload);
    let _ = call(&|req| Request::Send {
        req,
        to: me.clone(),
        channel: CHAT_CHANNEL,
        reliable: true,
        payload: b.clone(),
    })?;
    // Results arrive as events keyed by msg_id; take them through the client's queue.
    let (msg_id, targets) = match call(&|req| Request::Send {
        req,
        to: "all".into(),
        channel: CHAT_CHANNEL,
        reliable: true,
        payload: b.clone(),
    })? {
        ReplyJson::Sending { msg_id, targets } => (msg_id, targets),
        ReplyJson::Error { code } => return Err(std::io::Error::other(code)),
        other => return Err(std::io::Error::other(format!("send: {other:?}"))),
    };
    let total = targets.len() as u32;
    let (mut ok, mut failed) = (0u32, vec![]);
    let end = Instant::now() + timeout;
    while ok + (failed.len() as u32) < total && Instant::now() < end {
        match client.recv_timeout(Duration::from_millis(100)) {
            Some(ClientEvent::Event(Event::MessageResult {
                msg_id: id,
                to,
                status,
            })) if id == msg_id => {
                if status == "delivered" {
                    ok += 1
                } else {
                    failed.push((to, status))
                }
            }
            _ => {}
        }
    }
    Ok((ok, total, failed))
}
