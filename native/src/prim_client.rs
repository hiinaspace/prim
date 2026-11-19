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
use prim_client::{Client, ClientEvent};
use serde_json::Value;
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
    client: Option<Client>,
}

#[godot_api]
impl INode for PrimClient {
    fn init(base: Base<Node>) -> Self {
        Self { base, client: None }
    }

    fn process(&mut self, _: f64) {
        let mut events = Vec::new();
        if let Some(c) = &self.client {
            // Bounded per frame so a flood cannot stall rendering.
            while events.len() < 256 {
                match c.try_recv() {
                    Some(e) => events.push(e),
                    None => break,
                }
            }
        }
        for e in events {
            self.emit_client_event(e);
        }
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

    /// Default socket path: `$XDG_RUNTIME_DIR/prim/room.sock`.
    #[func]
    fn default_socket_path() -> GString {
        let p = match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
            Some(d) => std::path::PathBuf::from(d).join("prim").join("room.sock"),
            None => std::path::PathBuf::from("/tmp/prim/room.sock"),
        };
        p.to_string_lossy().as_ref().into()
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
        #[cfg(unix)]
        {
            self.client = Some(Client::start(UnixTransport::new(p)));
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
        self.client = None;
    }

    #[func]
    fn is_daemon_connected(&self) -> bool {
        self.client.as_ref().is_some_and(|c| c.is_connected())
    }

    /// Subscribe to a state pattern, e.g. "member/*/pose". Returns the subscription id,
    /// or -1 on error.
    #[func]
    fn subscribe(&mut self, pattern: GString) -> i64 {
        self.client
            .as_ref()
            .and_then(|c| c.subscribe(&pattern.to_string()).ok())
            .map_or(-1, i64::from)
    }

    #[func]
    fn unsubscribe(&mut self, sub: i64) {
        if let Some(c) = &self.client {
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
        self.client
            .as_ref()
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
        self.client
            .as_ref()
            .and_then(|c| c.register_channel(channel as u16).ok())
            .map_or(-1, |r| r as i64)
    }

    #[func]
    fn unregister_channel(&mut self, channel: i64) {
        if let Some(c) = &self.client {
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
        self.client
            .as_ref()
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
        self.client.as_ref().is_some_and(|c| {
            c.send_datagram(
                &to.to_string(),
                channel as u16,
                payload.as_slice(),
                max_age_ms.max(0) as u32,
            )
            .is_ok()
        })
    }

    /// Ask for the room status; the answer is a `reply` whose data has "status".
    #[func]
    fn request_status(&mut self) -> i64 {
        self.client
            .as_ref()
            .and_then(|c| c.status().ok())
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
