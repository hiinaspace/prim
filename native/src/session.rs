use crate::network::{Address, Event, Handle};
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
        match Handle::start(secret, name.to_string(), test_host, local_only) {
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
            .is_some_and(|h| h.shared.is_host() && h.shared.media.share(path.to_string()))
    }
    #[func]
    fn receive_file(&self, json: GString) -> bool {
        let Ok(d) = serde_json::from_str::<crate::media::Descriptor>(&json.to_string()) else {
            return false;
        };
        self.handle
            .as_ref()
            .is_some_and(|h| d.owner == *h.shared.host.read().unwrap() && h.shared.media.receive(d))
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
                serde_json::to_string(&*h.shared.media.view.lock().unwrap()).unwrap_or_default()
            })
            .unwrap_or_else(|| "{}".into())
            .as_str()
            .into()
    }
    #[func]
    fn allow_media_relay(&self, media_id: GString, peer: GString, allow: bool) {
        if let Some(h) = &self.handle {
            if h.shared.is_host() {
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
