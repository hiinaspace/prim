use crate::network::{Address, Event, Handle, Mode, Options, Policy};
use godot::prelude::*;
use godot_network_audio::{AudioStreamNetwork, NetworkAudioSender};
use std::collections::HashMap;

#[derive(GodotClass)]
#[class(base=Node)]
pub struct PrimSession {
    base: Base<Node>,
    handle: Option<Handle>,
    streams: HashMap<String, Gd<AudioStreamNetwork>>,
    info: GString,
    local_id: GString,
    host_id: GString,
    identity: Option<[u8; 32]>,
}
/// Persistent identity helpers: the endpoint ID is the person's Prim ID.
#[derive(GodotClass)]
#[class(base=RefCounted, init)]
pub struct PrimIdentity {
    base: Base<RefCounted>,
}
#[godot_api]
impl PrimIdentity {
    /// 32 random bytes usable as an identity key.
    #[func]
    fn generate_key() -> PackedByteArray {
        PackedByteArray::from(crate::network::generate_key().as_slice())
    }
    /// The friend code (endpoint ID) for a key, or "" if the key is not 32 bytes.
    #[func]
    fn key_id(key: PackedByteArray) -> GString {
        match <[u8; 32]>::try_from(key.as_slice()) {
            Ok(key) => crate::network::key_id(&key).as_str().into(),
            Err(_) => GString::new(),
        }
    }
    /// True if the text parses as an endpoint ID.
    #[func]
    fn is_valid_id(id: GString) -> bool {
        id.to_string().parse::<iroh::EndpointId>().is_ok()
    }
}
#[godot_api]
impl INode for PrimSession {
    fn init(base: Base<Node>) -> Self {
        Self {
            base,
            handle: None,
            streams: HashMap::new(),
            info: GString::new(),
            local_id: GString::new(),
            host_id: GString::new(),
            identity: None,
        }
    }
    fn process(&mut self, _: f64) {
        let events: Vec<Event> = self
            .handle
            .as_ref()
            .map(|h| h.events.try_iter().take(128).collect())
            .unwrap_or_default();
        for event in events {
            match event {
                Event::Status(status) => {
                    self.base_mut()
                        .emit_signal("status_changed", &[status.to_variant()]);
                }
                Event::Ready(info) => {
                    if let Ok(address) = serde_json::from_str::<Address>(&info) {
                        self.local_id = address.id.as_str().into();
                    }
                    self.info = info.as_str().into();
                    self.base_mut().emit_signal("endpoint_ready", &[]);
                }
                Event::Host(id) => {
                    self.host_id = id.as_str().into();
                    self.base_mut()
                        .emit_signal("host_changed", &[id.to_variant()]);
                }
                Event::Connected(id, name) => {
                    let stream = AudioStreamNetwork::new_gd();
                    if let Some(handle) = &self.handle {
                        handle
                            .shared
                            .ingress
                            .write()
                            .unwrap()
                            .insert(id.clone(), stream.bind().loopback_target());
                    }
                    self.streams.insert(id.clone(), stream);
                    self.base_mut()
                        .emit_signal("peer_connected", &[id.to_variant(), name.to_variant()]);
                }
                Event::Disconnected(id) => {
                    self.streams.remove(&id);
                    self.base_mut()
                        .emit_signal("peer_disconnected", &[id.to_variant()]);
                }
                Event::Message(id, message) => {
                    self.base_mut()
                        .emit_signal("message_received", &[id.to_variant(), message.to_variant()]);
                }
                Event::Error(message) => {
                    self.base_mut()
                        .emit_signal("network_error", &[message.to_variant()]);
                }
            }
        }
        let poses = self
            .handle
            .as_ref()
            .map(|h| std::mem::take(&mut *h.shared.poses.lock().unwrap()))
            .unwrap_or_default();
        for (id, bytes) in poses {
            self.base_mut().emit_signal(
                "pose_received",
                &[
                    id.to_variant(),
                    PackedByteArray::from(bytes.as_slice()).to_variant(),
                ],
            );
        }
        if self.handle.as_ref().is_some_and(|handle| handle.finished()) {
            self.handle = None;
            self.streams.clear();
            self.host_id = GString::new();
            self.local_id = GString::new();
            self.base_mut().emit_signal("left_room", &[]);
        }
    }
    fn exit_tree(&mut self) {
        self.handle = None;
        self.streams.clear();
    }
}
#[godot_api]
impl PrimSession {
    #[signal]
    fn status_changed(message: GString);
    #[signal]
    fn network_error(message: GString);
    #[signal]
    fn endpoint_ready();
    #[signal]
    fn host_changed(peer: GString);
    #[signal]
    fn peer_connected(peer: GString, name: GString);
    #[signal]
    fn peer_disconnected(peer: GString);
    #[signal]
    fn message_received(peer: GString, json: GString);
    #[signal]
    fn pose_received(peer: GString, bytes: PackedByteArray);
    #[signal]
    fn left_room();
    #[func]
    fn join_room(&mut self, secret: GString, name: GString, test_host_json: GString) -> bool {
        if self.handle.is_some() {
            return false;
        }
        let text = secret.to_string();
        if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
            self.base_mut().emit_signal(
                "network_error",
                &["Missing private room configuration".to_variant()],
            );
            return false;
        }
        let mut secret = [0u8; 32];
        for (i, byte) in secret.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).unwrap();
        }
        let local_only = std::env::var("PRIM_NETWORK_LOCAL_ONLY").as_deref() == Ok("1");
        let test_host = if test_host_json.is_empty() {
            None
        } else {
            match serde_json::from_str::<Address>(&test_host_json.to_string()) {
                Ok(addr) => Some(addr),
                Err(_) => return false,
            }
        };
        self.start(Options {
            secret,
            key: self.identity,
            name: name.to_string(),
            mode: Mode::Shared {
                direct_host: test_host,
            },
            local_only,
        })
    }
    fn start(&mut self, options: Options) -> bool {
        match Handle::start_with(options) {
            Ok(handle) => {
                self.handle = Some(handle);
                true
            }
            Err(err) => {
                self.base_mut()
                    .emit_signal("network_error", &[err.to_string().to_variant()]);
                false
            }
        }
    }
    /// Use this 32-byte key as the endpoint identity for sessions started later.
    #[func]
    fn set_identity(&mut self, key: PackedByteArray) -> bool {
        match <[u8; 32]>::try_from(key.as_slice()) {
            Ok(key) => {
                self.identity = Some(key);
                true
            }
            Err(_) => false,
        }
    }
    /// Host a fresh private instance. `mode` is "friends" or "invite"; `ids` is the
    /// initial admission set (endpoint IDs).
    #[func]
    fn host_instance(&mut self, mode: GString, ids: PackedStringArray, name: GString) -> bool {
        if self.handle.is_some() {
            return false;
        }
        let Some(policy) = Policy::parse(&mode.to_string()) else {
            return false;
        };
        let admitted = ids.as_slice().iter().map(|id| id.to_string()).collect();
        self.start(Options {
            secret: [0; 32],
            key: self.identity,
            name: name.to_string(),
            mode: Mode::Host { policy, admitted },
            local_only: std::env::var("PRIM_NETWORK_LOCAL_ONLY").as_deref() == Ok("1"),
        })
    }
    /// Join a friend's instance. `target` is a bare endpoint ID (resolved through
    /// iroh address lookup) or an Address JSON object (tests, direct addresses).
    #[func]
    fn join_friend(&mut self, target: GString, name: GString) -> bool {
        if self.handle.is_some() {
            return false;
        }
        let text = target.to_string();
        let text = text.trim();
        let host = if text.starts_with('{') {
            match serde_json::from_str::<Address>(text) {
                Ok(address) => address,
                Err(_) => return false,
            }
        } else if text.parse::<iroh::EndpointId>().is_ok() {
            Address {
                id: text.into(),
                relay: None,
                direct: Vec::new(),
            }
        } else {
            return false;
        };
        self.start(Options {
            secret: [0; 32],
            key: self.identity,
            name: name.to_string(),
            mode: Mode::Join { host },
            local_only: std::env::var("PRIM_NETWORK_LOCAL_ONLY").as_deref() == Ok("1"),
        })
    }
    /// Replace the live admission set of a hosted instance.
    #[func]
    fn set_admission(&self, ids: PackedStringArray) {
        if let Some(handle) = &self.handle {
            handle
                .shared
                .set_admission(ids.as_slice().iter().map(|id| id.to_string()).collect());
        }
    }
    #[func]
    fn leave_room(&mut self) {
        if let Some(handle) = &self.handle {
            handle.shared.stop();
        }
    }
    #[func]
    fn get_local_id(&self) -> GString {
        self.local_id.clone()
    }
    #[func]
    fn get_host_id(&self) -> GString {
        self.host_id.clone()
    }
    #[func]
    fn get_endpoint_info(&self) -> GString {
        self.info.clone()
    }
    #[func]
    fn is_active(&self) -> bool {
        self.handle.is_some()
    }
    #[func]
    fn is_host(&self) -> bool {
        !self.local_id.is_empty() && self.local_id == self.host_id
    }
    #[func]
    fn send_control(&self, peer: GString, json: GString) -> bool {
        self.handle
            .as_ref()
            .is_some_and(|h| h.shared.send_control(&peer.to_string(), json.to_string()))
    }
    #[func]
    fn broadcast_control(&self, json: GString) {
        for peer in self.streams.keys() {
            if let Some(handle) = &self.handle {
                handle.shared.send_control(peer, json.to_string());
            }
        }
    }
    #[func]
    fn share_file(&self, path: GString) -> bool {
        self.handle
            .as_ref()
            .is_some_and(|h| h.shared.media.share(path.to_string()))
    }
    #[func]
    fn receive_file(&self, json: GString) -> bool {
        let Ok(d) = serde_json::from_str::<crate::media::Descriptor>(&json.to_string()) else {
            return false;
        };
        self.handle
            .as_ref()
            .is_some_and(|h| h.shared.media_peer(&d.owner).is_some() && h.shared.media.receive(d))
    }
    #[func]
    fn publish_file(&self, id: GString) -> bool {
        self.handle
            .as_ref()
            .is_some_and(|h| h.shared.media.publish(&id.to_string()))
    }
    #[func]
    fn stop_receiving_file(&self) {
        if let Some(h) = &self.handle {
            h.shared.media.stop_receiving();
        }
    }
    #[func]
    fn stop_publishing_file(&self, id: GString) {
        if let Some(h) = &self.handle {
            h.shared.media.stop_publishing(&id.to_string());
        }
    }
    #[func]
    fn cancel_file_offer(&self) {
        if let Some(h) = &self.handle {
            h.shared.media.cancel_offer();
        }
    }
    #[func]
    fn validate_file_offer(&self, peer: GString, json: GString) -> bool {
        let Ok(d) = serde_json::from_str::<crate::media::Descriptor>(&json.to_string()) else {
            return false;
        };
        self.handle.as_ref().is_some_and(|h| {
            d.valid()
                && d.owner == peer.to_string()
                && (d.owner == *h.shared.local.read().unwrap()
                    || h.shared.media_peer(&d.owner).is_some())
        })
    }
    #[func]
    fn stop_sharing(&self) {
        if let Some(h) = &self.handle {
            h.shared.media.stop();
        }
    }
    #[func]
    fn media_state(&self) -> GString {
        self.handle
            .as_ref()
            .map(|h| {
                let active = h.shared.media.publication_active();
                let mut view =
                    serde_json::to_value(&*h.shared.media.view.lock().unwrap()).unwrap_or_default();
                view["publication_active"] = active.into();
                view.to_string()
            })
            .unwrap_or_else(|| "{}".into())
            .as_str()
            .into()
    }
    #[func]
    fn allow_media_relay(&self, media_id: GString, peer: GString, allow: bool) {
        if let Some(h) = &self.handle {
            {
                h.shared
                    .media
                    .consent(&media_id.to_string(), &peer.to_string(), allow);
            }
        }
    }
    #[func]
    fn set_media_upload_limit(&self, mbps: i64) {
        if let Some(h) = &self.handle {
            h.shared.media.upload_mbps.store(
                mbps.clamp(1, 1000) as u64,
                std::sync::atomic::Ordering::Release,
            );
        }
    }
    #[func]
    fn send_pose(&self, bytes: PackedByteArray) {
        if let Some(handle) = &self.handle {
            handle.shared.send_pose(bytes.as_slice());
        }
    }
    #[func]
    fn receive_stream(&self, peer: GString) -> Option<Gd<AudioStreamNetwork>> {
        self.streams.get(&peer.to_string()).cloned()
    }
    #[func]
    fn attach_sender(&self, mut sender: Gd<NetworkAudioSender>) {
        if let Some(handle) = &self.handle {
            let shared = Arc::downgrade(&handle.shared);
            sender
                .bind_mut()
                .install_direct_send_handler(Arc::new(move |bytes| {
                    if let Some(shared) = shared.upgrade() {
                        shared.send_voice(&bytes);
                    }
                }));
        }
    }
}
use std::sync::Arc;
