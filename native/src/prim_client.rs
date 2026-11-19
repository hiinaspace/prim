//! `PrimClient`: the Godot face of the room daemon's local API (room.sock).
//!
//! The socket I/O, reconnect and replay live in the plain `prim-client` crate (one worker
//! thread). This node polls that worker's queue in `_process` and emits Godot signals on
//! the main thread, which is the only thread Godot lets touch the scene. The old
//! `PrimSession` (network.rs) stays until round C1 replaces it.
//!
//! Also here: `PrimEvents`, the JSONL event sink for GDScript (see `prim-events`).

use godot::prelude::*;
use prim_client::transport::UnixTransport;
use prim_client::wire::{unb64, Event, ReplyJson, UpdateJson};
use prim_client::chat::Clients;
use prim_client::room::{Room, RoomOpts};
use prim_client::session::{SessionClient, SessionRequest, SessionStatus};
use prim_client::{Client, ClientEvent};
use serde_json::Value;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;

fn json_to_variant(v: &Value) -> Variant {
    match v {
        Value::Null => Variant::nil(),
        Value::Bool(b) => b.to_variant(),
        Value::Number(n) => match n.as_i64() {
            Some(i) => i.to_variant(),
            None => n.as_f64().unwrap_or(0.0).to_variant(),
        },
        Value::String(s) => GString::from(s.as_str()).to_variant(),
        Value::Array(a) => {
            let mut arr = VarArray::new();
            for x in a {
                arr.push(&json_to_variant(x));
            }
            arr.to_variant()
        }
        Value::Object(o) => {
            let mut d = VarDictionary::new();
            for (k, x) in o {
                d.set(&GString::from(k.as_str()).to_variant(), &json_to_variant(x));
            }
            d.to_variant()
        }
    }
}

fn dict_of(v: &Value) -> VarDictionary {
    json_to_variant(v).try_to().unwrap_or_default()
}

#[derive(GodotClass)]
#[class(base=Node)]
pub struct PrimClient {
    base: Base<Node>,
    /// The room model around the daemon connection (round A1): status, names, chat,
    /// default-name rule and pending requests live in `prim-client`, not in GDScript.
    room: Option<Room>,
    session_tx: Sender<(i64, Result<SessionStatus, String>)>,
    session_rx: Receiver<(i64, Result<SessionStatus, String>)>,
    next_session_req: i64,
    default_name: String,
    room_socket: Option<std::path::PathBuf>,
}

#[godot_api]
impl INode for PrimClient {
    fn init(base: Base<Node>) -> Self {
        let (session_tx, session_rx) = channel();
        Self {
            base,
            room: None,
            session_tx,
            session_rx,
            next_session_req: 1,
            default_name: String::new(),
            room_socket: None,
        }
    }

    fn process(&mut self, _: f64) {
        // The room model folds every event in (and sends its own follow-up requests); the
        // raw events still reach GDScript, except the model's own "**" subscription.
        let events = match &mut self.room {
            Some(r) => r.pump(),
            None => Vec::new(),
        };
        for e in events {
            let own = matches!(&e, ClientEvent::Event(Event::Updates { sub, .. })
                if self.room.as_ref().is_some_and(|r| r.owns_sub(*sub)));
            if !own {
                self.emit_client_event(e);
            }
        }
        while let Ok((id, res)) = self.session_rx.try_recv() {
            let d = match res {
                Ok(st) => {
                    let mut d = dict_of(&serde_json::to_value(&st).unwrap_or(Value::Null));
                    d.set(&GString::from("ok").to_variant(), &true.to_variant());
                    d
                }
                Err(m) => {
                    let mut d = VarDictionary::new();
                    d.set(&GString::from("ok").to_variant(), &false.to_variant());
                    d.set(&GString::from("error").to_variant(), &GString::from(m.as_str()).to_variant());
                    d
                }
            };
            self.base_mut()
                .emit_signal("session_reply", &[id.to_variant(), d.to_variant()]);
        }
    }
}

impl PrimClient {
    fn cl(&self) -> Option<&Client> {
        self.room.as_ref().map(|r| r.client())
    }
}

#[godot_api]
impl PrimClient {
    /// The daemon connected (and subscriptions/channels were replayed).
    #[signal]
    fn connected();
    #[signal]
    fn disconnected();
    /// Reply to a request id returned by write_state/send_message/request_status/...
    /// `data` is the reply as a Dictionary ("kind": ok|error|sending|status|invite ...).
    #[signal]
    fn reply(req: i64, data: VarDictionary);
    /// A state value: snapshot or delta. `writer` is the member key (hex).
    #[signal]
    fn state_value(sub: i64, path: GString, writer: GString, live: bool, value: PackedByteArray);
    #[signal]
    fn state_removed(sub: i64, path: GString);
    /// The subscription restarts (daemon reconnect, or lifecycle change): drop its state.
    #[signal]
    fn state_reset(sub: i64);
    #[signal]
    fn snapshot_done(sub: i64);
    #[signal]
    fn message_received(from: GString, channel: i64, payload: PackedByteArray);
    #[signal]
    fn datagram_received(from: GString, channel: i64, payload: PackedByteArray, age_ms: i64);
    /// status: delivered | no_channel | not_member | too_large | lost | offline
    #[signal]
    fn message_result(msg_id: i64, to: GString, status: GString);
    /// what: joined | online | offline | unresponsive | removed
    #[signal]
    fn member_event(key: GString, what: GString);
    #[signal]
    fn coordinator_event(change: GString, generation: i64, term: i64, holder: GString);
    #[signal]
    fn room_closed(generation: i64);
    #[signal]
    fn room_opened(generation: i64);
    /// This member was removed from the room.
    #[signal]
    fn removed();
    #[signal]
    fn daemon_error(message: GString);
    /// The answer to `session_request`: `ok`, and on success the session's status
    /// (room, kind, error, voice{running,wanted,mic,deaf,state,...}); `error` on failure.
    #[signal]
    fn session_reply(id: i64, data: VarDictionary);

    /// Default socket path: `$XDG_RUNTIME_DIR/prim/room.sock`.
    #[func]
    fn default_socket_path() -> GString {
        prim_client::paths::room_socket().to_string_lossy().as_ref().into()
    }

    /// Start the worker against a Unix socket. It connects (and reconnects) in the
    /// background; watch `connected`. Empty `path` = the default socket.
    #[func]
    fn connect_to(&mut self, path: GString) -> bool {
        let p = if path.is_empty() {
            Self::default_socket_path().to_string()
        } else {
            path.to_string()
        };
        self.room_socket = Some(std::path::PathBuf::from(&p));
        #[cfg(unix)]
        {
            let name = (!self.default_name.is_empty()).then(|| self.default_name.clone());
            self.room = Some(Room::start(
                UnixTransport::new(p),
                RoomOpts {
                    default_name: name,
                    chat: true,
                    repaint: None,
                },
            ));
            true
        }
        #[cfg(not(unix))]
        {
            let _ = p;
            false
        }
    }

    #[func]
    fn close(&mut self) {
        self.room = None;
    }

    #[func]
    fn is_daemon_connected(&self) -> bool {
        self.cl().is_some_and(|c| c.is_connected())
    }

    /// Subscribe to a state pattern, e.g. "member/*/pose". Returns the subscription id,
    /// or -1 on error.
    #[func]
    fn subscribe(&mut self, pattern: GString) -> i64 {
        self.cl()
            .and_then(|c| c.subscribe(&pattern.to_string()).ok())
            .map_or(-1, i64::from)
    }

    #[func]
    fn unsubscribe(&mut self, sub: i64) {
        if let Some(c) = self.cl() {
            let _ = c.unsubscribe(sub as u32);
        }
    }

    /// Write state. scope: "member" or "room". `live` records are latest-wins (<= 200 B);
    /// durable otherwise. Returns the request id (a `reply` follows), or -1 if down.
    #[func]
    fn write_state(
        &mut self,
        scope: GString,
        name: GString,
        live: bool,
        persistent: bool,
        value: PackedByteArray,
    ) -> i64 {
        self.cl()
            .and_then(|c| {
                c.write(
                    &scope.to_string(),
                    &name.to_string(),
                    live,
                    persistent,
                    value.as_slice(),
                )
                .ok()
            })
            .map_or(-1, |r| r as i64)
    }

    #[func]
    fn register_channel(&mut self, channel: i64) -> i64 {
        self.cl()
            .and_then(|c| c.register_channel(channel as u16).ok())
            .map_or(-1, |r| r as i64)
    }

    #[func]
    fn unregister_channel(&mut self, channel: i64) {
        if let Some(c) = self.cl() {
            let _ = c.unregister_channel(channel as u16);
        }
    }

    /// App message to "all", "coordinator" or a member key (hex). Returns the request id.
    #[func]
    fn send_message(
        &mut self,
        to: GString,
        channel: i64,
        reliable: bool,
        payload: PackedByteArray,
    ) -> i64 {
        self.cl()
            .and_then(|c| {
                c.send(&to.to_string(), channel as u16, reliable, payload.as_slice())
                    .ok()
            })
            .map_or(-1, |r| r as i64)
    }

    #[func]
    fn send_datagram(
        &mut self,
        to: GString,
        channel: i64,
        payload: PackedByteArray,
        max_age_ms: i64,
    ) -> bool {
        self.cl().is_some_and(|c| {
            c.send_datagram(
                &to.to_string(),
                channel as u16,
                payload.as_slice(),
                max_age_ms.max(0) as u32,
            )
            .is_ok()
        })
    }

    /// Mint a one-time invite token (reply kind "invite", field "token"). 0 = daemon default.
    #[func]
    fn create_invite(&mut self, expiry_ms: i64) -> i64 {
        self.cl()
            .and_then(|c| {
                c.request(|req| prim_client::wire::Request::CreateInvite {
                    req,
                    expiry_in_ms: expiry_ms.max(0) as u64,
                    relays: vec![],
                })
                .ok()
            })
            .map_or(-1, |r| r as i64)
    }

    /// Revoke a member (hex key) or an unused invite's key.
    #[func]
    fn revoke(&mut self, subject: GString) -> i64 {
        self.cl()
            .and_then(|c| {
                c.request(|req| prim_client::wire::Request::Revoke {
                    req,
                    subject: subject.to_string(),
                })
                .ok()
            })
            .map_or(-1, |r| r as i64)
    }

    /// Leave the room (the daemon then exits).
    #[func]
    fn leave(&mut self) -> i64 {
        self.cl()
            .and_then(|c| c.request(|req| prim_client::wire::Request::Leave { req }).ok())
            .map_or(-1, |r| r as i64)
    }

    // ---- the room model (round A1) -------------------------------------------------

    /// The name a member gets when it has none; call before `connect_to`. Empty = the
    /// machine's host name (`$PRIM_NAME` first).
    #[func]
    fn set_default_name(&mut self, name: GString) {
        self.default_name = if name.is_empty() {
            prim_client::chat::default_name()
        } else {
            name.to_string()
        };
    }

    /// Everything a menu shows, as one Dictionary: attached, in_room, lifecycle, room,
    /// room_name, me, my_name, join ("none"|"pending"|"refused"), join_refused, removed,
    /// notice, invite, voice_session, voice_incarnation, members[{key, name, is_me,
    /// presence, coordinator, listening, mic, deaf, ...}].
    #[func]
    fn room_view(&self) -> VarDictionary {
        match &self.room {
            Some(r) => dict_of(&r.model.view()),
            None => VarDictionary::new(),
        }
    }

    /// Chat lines with id >= `from_id`: [{id, from, name, text, system, unix, delivery}].
    #[func]
    fn chat_lines(&self, from_id: i64) -> VarArray {
        let mut out = VarArray::new();
        let Some(r) = &self.room else { return out };
        let m = &r.model;
        for l in m.chat.iter().filter(|l| l.id as i64 >= from_id) {
            let unix = l
                .at
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let v = serde_json::json!({
                "id": l.id, "from": l.from, "name": l.from.as_ref().map(|k| m.name_of(k)),
                "text": l.text, "system": l.from.is_none(), "unix": unix,
                "delivery": l.delivery,
            });
            out.push(&json_to_variant(&v));
        }
        out
    }

    #[func]
    fn chat_send(&mut self, text: GString) {
        if let Some(r) = &mut self.room {
            r.chat(&text.to_string());
        }
    }

    /// Write `member/<me>/profile`.
    #[func]
    fn set_display_name(&mut self, name: GString) {
        if let Some(r) = &mut self.room {
            r.set_name(&name.to_string());
        }
    }

    /// Mint an invite (one hour); it appears as `invite` in `room_view`.
    #[func]
    fn make_invite(&mut self, expiry_ms: i64) {
        if let Some(r) = &mut self.room {
            r.invite_for(if expiry_ms > 0 { expiry_ms as u64 } else { 3_600_000 });
        }
    }

    /// Tell the others which of my programs run (`member/<me>/clients`).
    #[func]
    fn set_clients(&mut self, godot: bool, player: bool, deaf: bool) {
        if let Some(r) = &mut self.room {
            r.set_clients(Clients { godot, player, deaf });
        }
    }

    // ---- the session supervisor (`prim session`) -----------------------------------

    /// Ask the session to do something, off the main thread: returns an id; the answer
    /// comes as `session_reply(id, data)`. `op`: ensure (start one if none runs), status,
    /// create (arg = room name), join (arg = invite), resume (arg = room id), leave,
    /// cancel, voice_on, voice_off, mic_on, mic_off, deafen_on, deafen_off, quit.
    #[func]
    fn session_request(&mut self, op: GString, arg: GString, extra: PackedStringArray) -> i64 {
        let id = self.next_session_req;
        self.next_session_req += 1;
        let tx = self.session_tx.clone();
        let (op, arg) = (op.to_string(), arg.to_string());
        let args: Vec<String> = extra.as_slice().iter().map(|s| s.to_string()).collect();
        let sock = self
            .room_socket
            .clone()
            .unwrap_or_else(prim_client::paths::room_socket);
        std::thread::spawn(move || {
            let sc = SessionClient::new(&sock);
            let req = match op.as_str() {
                "ensure" => {
                    let _ = tx.send((id, sc.ensure_running(&sock).map_err(|e| e.to_string())));
                    return;
                }
                "status" => SessionRequest::Status,
                "create" => SessionRequest::Create { name: arg, args },
                "join" => SessionRequest::Join { token: arg, args },
                "resume" => SessionRequest::Resume { room: arg, args },
                "leave" => SessionRequest::Leave,
                "cancel" => SessionRequest::Cancel,
                "voice_on" => SessionRequest::Voice { on: true },
                "voice_off" => SessionRequest::Voice { on: false },
                "mic_on" => SessionRequest::SetMic { on: true },
                "mic_off" => SessionRequest::SetMic { on: false },
                "deafen_on" => SessionRequest::SetDeafen { on: true },
                "deafen_off" => SessionRequest::SetDeafen { on: false },
                "quit" => SessionRequest::Quit,
                other => {
                    let _ = tx.send((id, Err(format!("unknown session op {other:?}"))));
                    return;
                }
            };
            let _ = tx.send((id, sc.call(&req).map_err(|e| e.to_string())));
        });
        id
    }

    /// Ask for the room status; the answer is a `reply` whose data has "status".
    #[func]
    fn request_status(&mut self) -> i64 {
        self.cl()
            .and_then(|c| c.status().ok())
            .map_or(-1, |r| r as i64)
    }

    /// Change the voice client's state without restarting it. `mic`: 1 live (unmute),
    /// 0 muted, -1 unchanged; `deaf`: 1 deafened, 0 hearing, -1 unchanged; `tray_live`:
    /// 1 / 0 / -1 (a lost tracking lease mutes the mic unless 1). The answer is a `reply`
    /// whose data is `{"kind": "voice", "voice": {mic, muted, deaf, listening, tray_live,
    /// incarnation, voice_running, ...}}`: what the voice process reported after applying
    /// the change, or `{"kind": "error", "code": "voice_not_running" | "voice_reconnecting" | "voice_no_ack"}`.
    /// Show that, not what you asked for. Returns the request id (-1: not connected).
    #[func]
    fn voice_control(&mut self, mic: i64, deaf: i64, tray_live: i64) -> i64 {
        let tri = |v: i64| (v >= 0).then_some(v != 0);
        self.client
            .as_ref()
            .and_then(|c| {
                c.request(|req| prim_client::wire::Request::VoiceControl {
                    req,
                    mic: tri(mic),
                    deaf: tri(deaf),
                    tray_live: tri(tray_live),
                })
                .ok()
            })
            .map_or(-1, |r| r as i64)
    }

    /// Ask what the voice client is doing now (same `reply` shape as `voice_control`).
    /// Call it after (re)connecting: voice keeps its state across Godot restarts.
    #[func]
    fn voice_status(&mut self) -> i64 {
        self.client
            .as_ref()
            .and_then(|c| {
                c.request(|req| prim_client::wire::Request::VoiceStatus { req })
                    .ok()
            })
            .map_or(-1, |r| r as i64)
    }

    fn emit_client_event(&mut self, e: ClientEvent) {
        let mut b = self.base_mut();
        match e {
            ClientEvent::Connected => {
                b.emit_signal("connected", &[]);
            }
            ClientEvent::Disconnected => {
                b.emit_signal("disconnected", &[]);
            }
            ClientEvent::Unknown(_) => {}
            ClientEvent::Event(ev) => match ev {
                Event::Reply { req, reply } => {
                    let v = serde_json::to_value(&reply).unwrap_or(Value::Null);
                    let _ = matches!(reply, ReplyJson::Ok);
                    b.emit_signal("reply", &[(req as i64).to_variant(), dict_of(&v).to_variant()]);
                }
                Event::Updates { sub, updates } => {
                    for u in updates {
                        let sub = sub as i64;
                        match u {
                            UpdateJson::Reset => {
                                b.emit_signal("state_reset", &[sub.to_variant()]);
                            }
                            UpdateJson::SnapshotDone => {
                                b.emit_signal("snapshot_done", &[sub.to_variant()]);
                            }
                            UpdateJson::Removed { path } => {
                                b.emit_signal(
                                    "state_removed",
                                    &[sub.to_variant(), GString::from(path.as_str()).to_variant()],
                                );
                            }
                            UpdateJson::Value { path, writer, class, value, .. } => {
                                let bytes = unb64(&value).unwrap_or_default();
                                b.emit_signal(
                                    "state_value",
                                    &[
                                        sub.to_variant(),
                                        GString::from(path.as_str()).to_variant(),
                                        GString::from(writer.as_str()).to_variant(),
                                        (class == "live").to_variant(),
                                        PackedByteArray::from(bytes.as_slice()).to_variant(),
                                    ],
                                );
                            }
                        }
                    }
                }
                Event::Message { from, channel, payload } => {
                    let bytes = unb64(&payload).unwrap_or_default();
                    b.emit_signal(
                        "message_received",
                        &[
                            GString::from(from.as_str()).to_variant(),
                            (channel as i64).to_variant(),
                            PackedByteArray::from(bytes.as_slice()).to_variant(),
                        ],
                    );
                }
                Event::Datagram { from, channel, payload, age_ms } => {
                    let bytes = unb64(&payload).unwrap_or_default();
                    b.emit_signal(
                        "datagram_received",
                        &[
                            GString::from(from.as_str()).to_variant(),
                            (channel as i64).to_variant(),
                            PackedByteArray::from(bytes.as_slice()).to_variant(),
                            (age_ms as i64).to_variant(),
                        ],
                    );
                }
                Event::MessageResult { msg_id, to, status } => {
                    b.emit_signal(
                        "message_result",
                        &[
                            (msg_id as i64).to_variant(),
                            GString::from(to.as_str()).to_variant(),
                            GString::from(status.as_str()).to_variant(),
                        ],
                    );
                }
                Event::Member { key, what } => {
                    b.emit_signal(
                        "member_event",
                        &[
                            GString::from(key.as_str()).to_variant(),
                            GString::from(what.as_str()).to_variant(),
                        ],
                    );
                }
                Event::Coordinator { change, generation, term, holder } => {
                    b.emit_signal(
                        "coordinator_event",
                        &[
                            GString::from(change.as_str()).to_variant(),
                            (generation as i64).to_variant(),
                            (term as i64).to_variant(),
                            GString::from(holder.as_str()).to_variant(),
                        ],
                    );
                }
                Event::RoomClosed { generation } => {
                    b.emit_signal("room_closed", &[(generation as i64).to_variant()]);
                }
                Event::RoomOpened { generation } => {
                    b.emit_signal("room_opened", &[(generation as i64).to_variant()]);
                }
                Event::Removed => {
                    b.emit_signal("removed", &[]);
                }
                Event::Error { message } => {
                    b.emit_signal(
                        "daemon_error",
                        &[GString::from(message.as_str()).to_variant()],
                    );
                }
            },
        }
    }
}

static SINK: Mutex<Option<prim_events::Sink>> = Mutex::new(None);

/// JSONL lifecycle events for GDScript. The sink opens `$PRIM_EVENT_LOG` on first use.
#[derive(GodotClass)]
#[class(base=RefCounted, init)]
pub struct PrimEvents {
    base: Base<RefCounted>,
}

#[godot_api]
impl PrimEvents {
    /// Record `event` (spawn, daemon_ready, joined, first_frame, first_voice, ...) with
    /// extra fields given as a JSON object string ("" or "{}" for none).
    #[func]
    fn emit(event: GString, fields_json: GString) {
        let fields: Value = serde_json::from_str(&fields_json.to_string()).unwrap_or(Value::Null);
        let mut g = SINK.lock().unwrap();
        g.get_or_insert_with(|| prim_events::Sink::from_env("godot"))
            .emit(&event.to_string(), fields);
    }

    /// CLOCK_MONOTONIC microseconds, comparable with the other components' events.
    #[func]
    fn mono_us() -> i64 {
        prim_events::mono_us() as i64
    }
}
