use anyhow::{bail, Context, Result};
use bytes::Bytes;
use godot_network_audio::NetworkAudioIngress;
use iroh::{
    endpoint::{presets, Connection},
    Endpoint, EndpointAddr, EndpointId, RelayUrl,
};
use iroh_base::TransportAddr;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, RwLock,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::mpsc;

const ALPN: &[u8] = b"prim/friends/1";
/// Identity-gated join: the host hands the instance secret to admitted IDs only.
pub const JOIN_ALPN: &[u8] = b"prim/join/1";
const AUTH: &[u8] = b"prim-friends-auth-v2";
const VERSION: u8 = 6;
const MAX_MESSAGE: usize = 16 * 1024;
const MAX_PEERS: usize = 5;
const VOICE: u8 = 1;
const POSE: u8 = 2;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Address {
    pub id: String,
    pub relay: Option<String>,
    pub direct: Vec<String>,
}
impl Address {
    fn from_endpoint(endpoint: &Endpoint) -> Self {
        let addr = endpoint.addr();
        let mut value = Self {
            id: addr.id.to_string(),
            relay: None,
            direct: Vec::new(),
        };
        for transport in &addr.addrs {
            match transport {
                TransportAddr::Ip(ip) => value.direct.push(ip.to_string()),
                TransportAddr::Relay(url) => value.relay = Some(url.to_string()),
                _ => (),
            }
        }
        value
    }
    pub(crate) fn parse(&self) -> Result<EndpointAddr> {
        let mut addr = EndpointAddr::new(self.id.parse::<EndpointId>()?);
        if let Some(url) = &self.relay {
            addr = addr.with_relay_url(url.parse::<RelayUrl>()?);
        }
        for ip in self.direct.iter().take(8) {
            addr = addr.with_ip_addr(ip.parse()?);
        }
        Ok(addr)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Hello {
    version: u8,
    proof: [u8; 32],
    address: Address,
    name: String,
    hosting: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Member {
    address: Address,
    name: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data")]
enum Wire {
    Members { host: String, members: Vec<Member> },
    App(String),
    Ping(u64),
    Pong(u64),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Record {
    version: u8,
    address: Address,
    expires: u64,
}
/// Who may enter a hosted instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    Friends,
    Invite,
}
impl Policy {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "friends" => Some(Self::Friends),
            "invite" => Some(Self::Invite),
            _ => None,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Friends => "friends",
            Self::Invite => "invite",
        }
    }
}
/// How a session finds its room.
#[derive(Clone, Debug)]
pub enum Mode {
    /// Existing behaviour: the build's lobby secret, DHT discovery or a test host.
    Shared { direct_host: Option<Address> },
    /// Fresh instance secret, no DHT record, admission by endpoint ID.
    Host {
        policy: Policy,
        admitted: Vec<String>,
    },
    /// Dial a host's `prim/join/1` to obtain its instance secret.
    Join { host: Address },
}
pub struct Options {
    /// Room secret for `Mode::Shared`; ignored by the other modes.
    pub secret: [u8; 32],
    /// Persistent endpoint secret key; random when absent.
    pub key: Option<[u8; 32]>,
    pub name: String,
    pub mode: Mode,
    pub local_only: bool,
}
/// `prim/join/1` reply from an admitting host.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct JoinReply {
    version: u8,
    secret: String,
    host: Address,
    mode: String,
    name: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct JoinRequest {
    version: u8,
}
pub fn generate_key() -> [u8; 32] {
    iroh_base::SecretKey::generate().to_bytes()
}
pub fn key_id(key: &[u8; 32]) -> String {
    iroh_base::SecretKey::from_bytes(key).public().to_string()
}
fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn from_hex(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}
#[derive(Debug)]
pub enum Event {
    Status(String),
    Ready(String),
    Host(String),
    Connected(String, String),
    Disconnected(String),
    Message(String, String),
    Error(String),
}
struct Peer {
    connection: Connection,
    send: mpsc::Sender<Wire>,
    member: Member,
}
pub struct Shared {
    peers: RwLock<HashMap<String, Peer>>,
    pub media: Arc<crate::media::Media>,
    pub ingress: RwLock<HashMap<String, NetworkAudioIngress>>,
    pub poses: Mutex<HashMap<String, Vec<u8>>>,
    pub host: RwLock<String>,
    pub local: RwLock<String>,
    stopped: AtomicBool,
    /// Policy of the instance this session hosts; `None` when not hosting one.
    policy: Option<Policy>,
    admitted: RwLock<HashSet<String>>,
    /// The room secret in use (instance secret once hosting or joined).
    pub(crate) room_secret: RwLock<Option<[u8; 32]>>,
    events: std::sync::mpsc::SyncSender<Event>,
    name: String,
}
impl Shared {
    fn event(&self, event: Event) {
        // Overflow cannot silently lose membership or host transitions. Retire the
        // session; the Godot owner will observe worker completion and reset.
        if self.events.try_send(event).is_err() {
            self.stop();
        }
    }
    /// Replace the live admission set; takes effect for the next join.
    pub fn set_admission(&self, ids: Vec<String>) {
        *self.admitted.write().unwrap() = ids.into_iter().collect();
    }
    fn admits(&self, id: &str) -> bool {
        self.admitted.read().unwrap().contains(id)
    }
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        self.media.stop();
    }
    pub fn send_control(&self, peer: &str, data: String) -> bool {
        if data.len() > MAX_MESSAGE / 2 {
            return false;
        }
        self.peers
            .read()
            .unwrap()
            .get(peer)
            .is_some_and(|p| p.send.try_send(Wire::App(data)).is_ok())
    }
    pub fn broadcast(&self, kind: u8, payload: &[u8]) {
        if payload.len() > 1100 {
            return;
        }
        let mut bytes = Vec::with_capacity(payload.len() + 1);
        bytes.push(kind);
        bytes.extend_from_slice(payload);
        let bytes = Bytes::from(bytes);
        for peer in self.peers.read().unwrap().values() {
            let _ = peer.connection.send_datagram(bytes.clone());
        }
    }
    pub fn send_voice(&self, payload: &[u8]) {
        self.broadcast(VOICE, payload);
    }
    pub fn send_pose(&self, payload: &[u8]) {
        self.broadcast(POSE, payload);
    }
    fn set_host(&self, host: String) {
        if *self.host.read().unwrap() == host {
            return;
        }
        *self.host.write().unwrap() = host.clone();
        self.event(Event::Host(host));
    }
    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
    pub(crate) fn media_peer(&self, id: &str) -> Option<(Address, String)> {
        self.peers
            .read()
            .unwrap()
            .get(id)
            .map(|p| (p.member.address.clone(), p.member.name.clone()))
    }
    pub(crate) fn is_host(&self) -> bool {
        let local = self.local.read().unwrap();
        !local.is_empty() && *self.host.read().unwrap() == *local
    }
}
pub struct Handle {
    pub shared: Arc<Shared>,
    pub events: std::sync::mpsc::Receiver<Event>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Handle {
    #[cfg(test)]
    pub fn start(
        secret: [u8; 32],
        name: String,
        direct_host: Option<Address>,
        local_only: bool,
    ) -> Result<Self> {
        Self::start_with(Options {
            secret,
            key: None,
            name,
            mode: Mode::Shared { direct_host },
            local_only,
        })
    }
    pub fn start_with(options: Options) -> Result<Self> {
        let Options {
            secret,
            key,
            name,
            mode,
            local_only,
        } = options;
        let (policy, admitted) = match &mode {
            Mode::Host { policy, admitted } => (Some(*policy), admitted.iter().cloned().collect()),
            _ => (None, HashSet::new()),
        };
        let (tx, events) = std::sync::mpsc::sync_channel(256);
        let shared = Arc::new(Shared {
            peers: RwLock::new(HashMap::new()),
            media: crate::media::Media::new(),
            ingress: RwLock::new(HashMap::new()),
            poses: Mutex::new(HashMap::new()),
            host: RwLock::new(String::new()),
            local: RwLock::new(String::new()),
            stopped: AtomicBool::new(false),
            policy,
            admitted: RwLock::new(admitted),
            room_secret: RwLock::new(None),
            events: tx,
            name: name.chars().take(32).collect(),
        });
        let state = shared.clone();
        let worker = std::thread::Builder::new()
            .name("prim-network".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build();
                match runtime {
                    Ok(runtime) => {
                        if let Err(err) =
                            runtime.block_on(run(state.clone(), secret, key, mode, local_only))
                        {
                            state.event(Event::Error(format!("Network: {err:#}")));
                        }
                    }
                    Err(err) => state.event(Event::Error(err.to_string())),
                }
                state.event(Event::Status("Offline".into()));
            })?;
        Ok(Self {
            shared,
            events,
            worker: Some(worker),
        })
    }
    pub fn finished(&self) -> bool {
        self.worker
            .as_ref()
            .is_none_or(|worker| worker.is_finished())
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.shared.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
async fn write_frame<T: Serialize>(
    stream: &mut iroh::endpoint::SendStream,
    value: &T,
) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_MESSAGE {
        bail!("message too large");
    }
    stream
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .await?;
    stream.write_all(&bytes).await?;
    Ok(())
}
async fn read_frame<T: for<'a> Deserialize<'a>>(
    stream: &mut iroh::endpoint::RecvStream,
) -> Result<T> {
    let mut size = [0; 4];
    stream.read_exact(&mut size).await?;
    let size = u32::from_be_bytes(size) as usize;
    if size > MAX_MESSAGE {
        bail!("message too large");
    }
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}
/// Room-secret MAC over TLS exporter material for one side of a connection.
/// The role is the exporter context, so the dialer's and acceptor's proofs
/// differ and neither side can pass by echoing the proof it received.
pub(crate) fn proof(
    connection: &Connection,
    secret: &[u8; 32],
    label: &[u8],
    dialer: bool,
) -> Result<blake3::Hash> {
    let role: &[u8] = if dialer { b"dialer" } else { b"acceptor" };
    let mut material = [0; 32];
    connection
        .export_keying_material(&mut material, label, role)
        .map_err(|_| anyhow::anyhow!("TLS key export failed"))?;
    Ok(blake3::keyed_hash(secret, &material))
}
async fn establish(
    endpoint: Endpoint,
    connection: Connection,
    outgoing: bool,
    state: Arc<Shared>,
    secret: [u8; 32],
) -> Result<()> {
    // Defense in depth: a hosted instance's secret only reaches admitted IDs, but
    // refuse unadmitted dialers even if it leaks.
    if !outgoing && state.policy.is_some() && !state.admits(&connection.remote_id().to_string()) {
        connection.close(3u8.into(), b"not admitted");
        bail!("room connection from unadmitted endpoint refused");
    }
    let own = proof(&connection, &secret, AUTH, outgoing)?;
    let expected = proof(&connection, &secret, AUTH, !outgoing)?;
    let hello = Hello {
        version: VERSION,
        proof: *own.as_bytes(),
        address: Address::from_endpoint(&endpoint),
        name: state.name.clone(),
        hosting: state.is_host(),
    };
    let (mut send, mut recv) = if outgoing {
        connection.open_bi().await?
    } else {
        connection.accept_bi().await?
    };
    // The dialer speaks first; the acceptor answers only after the dialer's proof
    // checks out, so an unauthenticated dialer learns nothing about the member.
    if outgoing {
        write_frame(&mut send, &hello).await?;
    }
    let remote: Hello = read_frame(&mut recv).await?;
    if remote.version != VERSION {
        connection.close(1u8.into(), b"update prim: incompatible version");
        bail!("Incompatible Prim version; update all clients");
    }
    // blake3::Hash equality is constant-time.
    if blake3::Hash::from(remote.proof) != expected
        || remote.address.id != connection.remote_id().to_string()
    {
        connection.close(1u8.into(), b"incompatible room");
        bail!("room authentication failed");
    }
    if !outgoing {
        write_frame(&mut send, &hello).await?;
    }
    let id = remote.address.id.clone();
    let (tx, mut rx) = mpsc::channel(64);
    {
        let mut peers = state.peers.write().unwrap();
        if peers.contains_key(&id) || peers.len() >= MAX_PEERS {
            connection.close(2u8.into(), b"duplicate or full");
            return Ok(());
        }
        peers.insert(
            id.clone(),
            Peer {
                connection: connection.clone(),
                send: tx,
                member: Member {
                    address: remote.address,
                    name: remote.name.chars().take(32).collect(),
                },
            },
        );
    }
    if remote.hosting && (!state.is_host() || id < endpoint.id().to_string()) {
        state.set_host(id.clone());
    }
    state.event(Event::Connected(
        id.clone(),
        remote.name.chars().take(32).collect(),
    ));
    publish_members(&endpoint, &state);
    let writer_state = state.clone();
    let writer_id = id.clone();
    tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            if write_frame(&mut send, &message).await.is_err() {
                break;
            }
        }
        let _ = (writer_state, writer_id);
    });
    let reader_state = state.clone();
    let reader_id = id.clone();
    let reader_endpoint = endpoint.clone();
    tokio::spawn(async move {
        while let Ok(message) = read_frame::<Wire>(&mut recv).await {
            match message {
                Wire::App(message) => {
                    reader_state.event(Event::Message(reader_id.clone(), message))
                }
                Wire::Members { host, members }
                    if host == reader_id && *reader_state.host.read().unwrap() == reader_id =>
                {
                    for member in members.into_iter().take(6) {
                        if member.address.id != reader_endpoint.id().to_string()
                            && reader_endpoint.id().to_string() < member.address.id
                            && !reader_state
                                .peers
                                .read()
                                .unwrap()
                                .contains_key(&member.address.id)
                        {
                            connect(
                                reader_endpoint.clone(),
                                member.address,
                                reader_state.clone(),
                                secret,
                            );
                        }
                    }
                }
                Wire::Ping(value) => {
                    if let Some(peer) = reader_state.peers.read().unwrap().get(&reader_id) {
                        let _ = peer.send.try_send(Wire::Pong(value));
                    }
                }
                _ => (),
            }
        }
    });
    tokio::spawn(async move {
        while let Ok(bytes) = connection.read_datagram().await {
            if bytes.is_empty() || bytes.len() > 1101 {
                continue;
            }
            match bytes[0] {
                VOICE => {
                    if let Some(ingress) = state.ingress.read().unwrap().get(&id) {
                        ingress.enqueue_bytes_at(&bytes[1..], Instant::now());
                    }
                }
                POSE => {
                    state
                        .poses
                        .lock()
                        .unwrap()
                        .insert(id.clone(), bytes[1..].to_vec());
                }
                _ => (),
            }
        }
        let removed = {
            let mut peers = state.peers.write().unwrap();
            if peers
                .get(&id)
                .is_some_and(|p| p.connection.stable_id() == connection.stable_id())
            {
                peers.remove(&id).is_some()
            } else {
                false
            }
        };
        if removed {
            state.ingress.write().unwrap().remove(&id);
            state.poses.lock().unwrap().remove(&id);
            state.event(Event::Disconnected(id.clone()));
            if *state.host.read().unwrap() == id {
                state.set_host(String::new());
                state.stop();
            } else {
                publish_members(&endpoint, &state);
            }
        }
    });
    Ok(())
}
fn connect(endpoint: Endpoint, address: Address, state: Arc<Shared>, secret: [u8; 32]) {
    tokio::spawn(async move {
        let result = tokio::time::timeout(Duration::from_secs(8), async {
            let connection = endpoint.connect(address.parse()?, ALPN).await?;
            establish(endpoint, connection, true, state.clone(), secret).await
        })
        .await;
        match result {
            Ok(Ok(())) => (),
            Ok(Err(err)) => state.event(Event::Error(format!("Connect: {err:#}"))),
            Err(_) => state.event(Event::Error("Connection timed out".into())),
        }
    });
}
fn publish_members(endpoint: &Endpoint, state: &Arc<Shared>) {
    if !state.is_host() {
        return;
    }
    let peers = state.peers.read().unwrap();
    let mut members = vec![Member {
        address: Address::from_endpoint(endpoint),
        name: state.name.clone(),
    }];
    members.extend(peers.values().map(|peer| peer.member.clone()));
    for peer in peers.values() {
        let _ = peer.send.try_send(Wire::Members {
            host: endpoint.id().to_string(),
            members: members.clone(),
        });
    }
}
fn close_text(connection: &Connection) -> Option<String> {
    match connection.close_reason() {
        Some(iroh::endpoint::ConnectionError::ApplicationClosed(close)) => {
            Some(String::from_utf8_lossy(&close.reason).into_owned())
        }
        _ => None,
    }
}
/// Dial a host's `prim/join/1` and receive its instance secret.
async fn request_join(endpoint: &Endpoint, host: &Address) -> Result<JoinReply> {
    let connection = endpoint.connect(host.parse()?, JOIN_ALPN).await?;
    let result: Result<JoinReply> = async {
        let (mut send, mut recv) = connection.open_bi().await?;
        write_frame(&mut send, &JoinRequest { version: VERSION }).await?;
        read_frame(&mut recv).await
    }
    .await;
    match result {
        Ok(reply) => {
            connection.close(0u8.into(), b"joined");
            if reply.version != VERSION {
                bail!("Incompatible Prim version; update all clients");
            }
            if reply.host.id != connection.remote_id().to_string() {
                bail!("join reply from unexpected endpoint");
            }
            Ok(reply)
        }
        Err(err) => {
            // The host explains refusals in the close reason.
            let reason = match close_text(&connection) {
                Some(reason) => Some(reason),
                None => tokio::time::timeout(Duration::from_secs(1), connection.closed())
                    .await
                    .ok()
                    .and(close_text(&connection)),
            };
            match reason {
                Some(reason) => bail!("{reason}"),
                None => Err(err),
            }
        }
    }
}
/// Host side of `prim/join/1`: admit by authenticated remote ID only.
async fn accept_join(connection: Connection, endpoint: Endpoint, state: Arc<Shared>) -> Result<()> {
    let Some(policy) = state.policy else {
        connection.close(5u8.into(), b"not hosting");
        return Ok(());
    };
    let remote = connection.remote_id().to_string();
    if !state.admits(&remote) {
        connection.close(3u8.into(), b"not admitted");
        return Ok(());
    }
    let secret = (*state.room_secret.read().unwrap()).context("no instance secret")?;
    let (mut send, mut recv) = connection.accept_bi().await?;
    let request: JoinRequest = read_frame(&mut recv).await?;
    if request.version != VERSION {
        connection.close(1u8.into(), b"update prim: incompatible version");
        return Ok(());
    }
    write_frame(
        &mut send,
        &JoinReply {
            version: VERSION,
            secret: to_hex(&secret),
            host: Address::from_endpoint(&endpoint),
            mode: policy.label().into(),
            name: state.name.clone(),
        },
    )
    .await?;
    let _ = send.finish();
    let _ = tokio::time::timeout(Duration::from_secs(5), connection.closed()).await;
    Ok(())
}
async fn run(
    state: Arc<Shared>,
    shared_secret: [u8; 32],
    key: Option<[u8; 32]>,
    mode: Mode,
    local_only: bool,
) -> Result<()> {
    state.event(Event::Status("Starting network…".into()));
    let alpns = vec![
        ALPN.to_vec(),
        JOIN_ALPN.to_vec(),
        crate::media::ALPN.to_vec(),
    ];
    let mut secret = shared_secret;
    // Hosting needs the instance secret before the accept loop starts.
    if matches!(mode, Mode::Host { .. }) {
        secret = generate_key();
    }
    *state.room_secret.write().unwrap() = Some(secret);
    let endpoint = {
        let secret_key = key.map(|k| iroh_base::SecretKey::from_bytes(&k));
        macro_rules! bind {
            ($preset:expr) => {{
                let mut builder = Endpoint::builder($preset).alpns(alpns);
                if let Some(secret_key) = secret_key {
                    builder = builder.secret_key(secret_key);
                }
                builder.bind().await?
            }};
        }
        if local_only {
            bind!(presets::Minimal)
        } else {
            // N0 publishes this endpoint to n0's address lookup and resolves other
            // IDs through it, so dialing a bare EndpointId works across the internet.
            bind!(presets::N0)
        }
    };
    *state.local.write().unwrap() = endpoint.id().to_string();
    if !local_only {
        let _ = tokio::time::timeout(Duration::from_secs(10), endpoint.online()).await;
    }
    state.event(Event::Ready(serde_json::to_string(
        &Address::from_endpoint(&endpoint),
    )?));
    // A joiner learns its room secret from the host before the room can start.
    let mut direct_host = None;
    match &mode {
        Mode::Join { host } => {
            state.event(Event::Status("Joining…".into()));
            let reply =
                match tokio::time::timeout(Duration::from_secs(15), request_join(&endpoint, host))
                    .await
                {
                    Ok(Ok(reply)) => reply,
                    Ok(Err(err)) => {
                        state.event(Event::Error(format!("Join: {err:#}")));
                        endpoint.close().await;
                        return Ok(());
                    }
                    Err(_) => {
                        state.event(Event::Error("Join: host did not answer".into()));
                        endpoint.close().await;
                        return Ok(());
                    }
                };
            let Some(joined) = from_hex(&reply.secret) else {
                state.event(Event::Error("Join: malformed reply".into()));
                endpoint.close().await;
                return Ok(());
            };
            secret = joined;
            *state.room_secret.write().unwrap() = Some(secret);
            direct_host = Some(reply.host);
        }
        Mode::Shared { direct_host: d } => direct_host = d.clone(),
        Mode::Host { .. } => (),
    }
    let media_task = tokio::spawn(
        state
            .media
            .clone()
            .run(endpoint.clone(), state.clone(), secret),
    );
    if let Some(host) = direct_host {
        connect(endpoint.clone(), host, state.clone(), secret);
    } else if matches!(mode, Mode::Host { .. }) || local_only {
        state.set_host(endpoint.id().to_string());
        if matches!(mode, Mode::Host { .. }) {
            state.event(Event::Status("Hosting".into()));
        }
    } else {
        let e = endpoint.clone();
        let s = state.clone();
        tokio::spawn(async move {
            if let Err(err) = discover(e, s.clone(), secret).await {
                s.event(Event::Error(format!("Lobby: {err:#}")));
                s.stop();
            }
        });
    }
    loop {
        tokio::select! {
            incoming = endpoint.accept() => {
                if let Some(incoming) = incoming {
                    let e = endpoint.clone(); let s = state.clone();
                    tokio::spawn(async move {
                        let Ok(Ok(conn)) = tokio::time::timeout(Duration::from_secs(8), incoming).await else { return; };
                        if conn.alpn() == crate::media::ALPN {
                            let _ = crate::media::accept(conn, s, secret).await;
                        } else if conn.alpn() == JOIN_ALPN {
                            let _ = accept_join(conn, e, s).await;
                        } else {
                            let _ = tokio::time::timeout(Duration::from_secs(8), establish(e,conn,false,s,secret)).await;
                        }
                    });
                }
            },
            _ = tokio::time::sleep(Duration::from_millis(50)) => { if state.stopped.load(Ordering::Acquire) { break; } }
        }
    }
    state.media.stop();
    media_task.abort();
    state.peers.write().unwrap().clear();
    state.ingress.write().unwrap().clear();
    state.poses.lock().unwrap().clear();
    let _ = tokio::time::timeout(Duration::from_secs(2), endpoint.close()).await;
    Ok(())
}
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
async fn discover(endpoint: Endpoint, state: Arc<Shared>, secret: [u8; 32]) -> Result<()> {
    let derived = blake3::derive_key("prim private friends lobby v1", &secret);
    let key = pkarr::Keypair::from_secret_key(&derived);
    let client = pkarr::Client::builder().no_relays().build()?;
    state.event(Event::Status("Looking for friends…".into()));
    for attempt in 0..2 {
        if state.stopped.load(Ordering::Acquire) {
            return Ok(());
        }
        if let Ok(Some(packet)) = tokio::time::timeout(
            Duration::from_secs(12),
            client.resolve_most_recent(&key.public_key()),
        )
        .await
        {
            for answer in packet.all_resource_records() {
                if let pkarr::dns::rdata::RData::TXT(txt) = &answer.rdata {
                    let raw: String = txt.clone().try_into().context("lobby record")?;
                    if let Ok(record) = serde_json::from_str::<Record>(&raw) {
                        if record.version == 1
                            && record.expires > now_secs()
                            && record.address.id != endpoint.id().to_string()
                        {
                            state.event(Event::Status("Connecting to friends…".into()));
                            connect(endpoint.clone(), record.address, state.clone(), secret);
                            for _ in 0..90 {
                                if !state.host.read().unwrap().is_empty() {
                                    return Ok(());
                                }
                                if state.stopped.load(Ordering::Acquire) {
                                    return Ok(());
                                }
                                tokio::time::sleep(Duration::from_millis(100)).await;
                            }
                        }
                    }
                }
            }
        }
        if attempt == 0 {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
    if state.stopped.load(Ordering::Acquire) {
        return Ok(());
    }
    state.set_host(endpoint.id().to_string());
    state.event(Event::Status("Hosting friends lobby".into()));
    while !state.stopped.load(Ordering::Acquire) && state.is_host() {
        let mut address = Address::from_endpoint(&endpoint);
        address.direct.truncate(4); // Keep the signed DNS packet comfortably bounded.
        let record = Record {
            version: 1,
            address,
            expires: now_secs() + 90,
        };
        let json = serde_json::to_string(&record)?;
        let packet = pkarr::SignedPacket::builder()
            .txt(".".try_into()?, json.as_str().try_into()?, 30)
            .sign(&key)?;
        match tokio::time::timeout(Duration::from_secs(15), client.publish(&packet, None)).await {
            Ok(Ok(())) => state.event(Event::Status("Friends lobby published".into())),
            _ => state.event(Event::Status("Hosting; lobby publication retrying…".into())),
        }
        for _ in 0..200 {
            if state.stopped.load(Ordering::Acquire) || !state.is_host() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn wait(handle: &Handle, predicate: impl Fn(&Event) -> bool) -> Event {
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            let event = handle
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("session event timed out");
            if predicate(&event) {
                return event;
            }
        }
    }
    #[test]
    fn local_mesh_routes_control_pose_and_departure() {
        let host = Handle::start([7; 32], "Host".into(), None, true).unwrap();
        let Event::Ready(info) = wait(&host, |e| matches!(e, Event::Ready(_))) else {
            unreachable!()
        };
        wait(&host, |e| matches!(e, Event::Host(_)));
        let addr: Address = serde_json::from_str(&info).unwrap();
        let host_id = addr.id.clone();
        let clients: Vec<_> = (0..5)
            .map(|i| Handle::start([7; 32], format!("Guest{i}"), Some(addr.clone()), true).unwrap())
            .collect();
        for client in &clients {
            wait(client, |e| matches!(e, Event::Host(id) if *id == host_id));
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        while clients
            .iter()
            .any(|c| c.shared.peers.read().unwrap().len() != 5)
            || host.shared.peers.read().unwrap().len() != 5
        {
            assert!(
                Instant::now() < deadline,
                "full mesh did not form: {:?}",
                clients
                    .iter()
                    .map(|c| c.shared.peers.read().unwrap().len())
                    .collect::<Vec<_>>()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let sender = clients[0].shared.local.read().unwrap().clone();
        assert!(clients[0]
            .shared
            .send_control(&host_id, "{\"action\":\"pause\"}".into()));
        wait(
            &host,
            |e| matches!(e, Event::Message(id,m) if *id == sender && m.contains("pause")),
        );
        clients[0].shared.send_pose(b"pose-marker");
        let deadline = Instant::now() + Duration::from_secs(3);
        while !host.shared.poses.lock().unwrap().contains_key(&sender) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(host.shared.poses.lock().unwrap()[&sender], b"pose-marker");
        assert!(!clients[0]
            .shared
            .send_control(&host_id, "x".repeat(MAX_MESSAGE)));
        clients[0].shared.stop();
        wait(
            &host,
            |e| matches!(e, Event::Disconnected(id) if *id == sender),
        );
        host.shared.stop();
        for client in clients.iter().skip(1) {
            wait(client, |e| matches!(e, Event::Host(id) if id.is_empty()));
        }
    }
    fn hosted(key: [u8; 32], policy: Policy, admitted: &[[u8; 32]]) -> (Handle, Address) {
        let host = Handle::start_with(Options {
            secret: [0; 32],
            key: Some(key),
            name: "Host".into(),
            mode: Mode::Host {
                policy,
                admitted: admitted.iter().map(key_id).collect(),
            },
            local_only: true,
        })
        .unwrap();
        let Event::Ready(info) = wait(&host, |e| matches!(e, Event::Ready(_))) else {
            unreachable!()
        };
        wait(&host, |e| matches!(e, Event::Host(_)));
        (host, serde_json::from_str(&info).unwrap())
    }
    fn joiner(key: [u8; 32], name: &str, host: &Address) -> Handle {
        Handle::start_with(Options {
            secret: [0; 32],
            key: Some(key),
            name: name.into(),
            mode: Mode::Join { host: host.clone() },
            local_only: true,
        })
        .unwrap()
    }
    fn peer_ids(handle: &Handle) -> Vec<String> {
        let mut ids: Vec<_> = handle
            .shared
            .peers
            .read()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        ids.sort();
        ids
    }
    fn wait_peers(handle: &Handle, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while handle.shared.peers.read().unwrap().len() != count {
            assert!(Instant::now() < deadline, "peers never reached {count}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    #[test]
    fn same_key_gives_same_endpoint_id_across_starts() {
        let key = generate_key();
        let ready_id = |key: Option<[u8; 32]>| {
            let handle = Handle::start_with(Options {
                secret: [3; 32],
                key,
                name: "A".into(),
                mode: Mode::Shared { direct_host: None },
                local_only: true,
            })
            .unwrap();
            let Event::Ready(info) = wait(&handle, |e| matches!(e, Event::Ready(_))) else {
                unreachable!()
            };
            serde_json::from_str::<Address>(&info).unwrap().id
        };
        let first = ready_id(Some(key));
        assert_eq!(first, key_id(&key));
        assert_eq!(first, ready_id(Some(key)));
        assert_ne!(first, ready_id(None));
        assert_ne!(first, ready_id(Some(generate_key())));
    }
    #[test]
    fn friends_host_admits_listed_keys_and_refuses_others() {
        let (kh, k1, k2, k3) = (
            generate_key(),
            generate_key(),
            generate_key(),
            generate_key(),
        );
        let (host, addr) = hosted(kh, Policy::Friends, &[k1, k2]);
        assert_eq!(addr.id, key_id(&kh));
        let (j1, j2) = (joiner(k1, "One", &addr), joiner(k2, "Two", &addr));
        // Full mesh: the host sees both, each joiner sees the host and the other.
        for handle in [&j1, &j2] {
            wait(handle, |e| matches!(e, Event::Host(id) if *id == addr.id));
            wait_peers(handle, 2);
        }
        wait_peers(&host, 2);
        assert_eq!(peer_ids(&host), {
            let mut v = vec![key_id(&k1), key_id(&k2)];
            v.sort();
            v
        });
        // An unlisted key is refused at the join ALPN and never appears anywhere.
        let intruder = joiner(k3, "Intruder", &addr);
        wait(
            &intruder,
            |e| matches!(e, Event::Error(m) if m.contains("not admitted")),
        );
        // Even holding the leaked instance secret, the room ALPN refuses it too.
        let secret = host.shared.room_secret.read().unwrap().unwrap();
        let leaked = Handle::start_with(Options {
            secret,
            key: Some(k3),
            name: "Leaked".into(),
            mode: Mode::Shared {
                direct_host: Some(addr.clone()),
            },
            local_only: true,
        })
        .unwrap();
        wait(&leaked, |e| matches!(e, Event::Error(_)));
        std::thread::sleep(Duration::from_millis(300));
        for handle in [&intruder, &leaked] {
            assert!(handle.shared.peers.read().unwrap().is_empty());
        }
        for handle in [&host, &j1, &j2] {
            assert!(!peer_ids(handle).contains(&key_id(&k3)));
            assert_eq!(handle.shared.peers.read().unwrap().len(), 2);
        }
    }
    #[test]
    fn invite_host_refuses_until_admission_is_updated() {
        let (kh, kg) = (generate_key(), generate_key());
        let (host, addr) = hosted(kh, Policy::Invite, &[]);
        let refused = joiner(kg, "Guest", &addr);
        wait(
            &refused,
            |e| matches!(e, Event::Error(m) if m.contains("not admitted")),
        );
        assert!(host.shared.peers.read().unwrap().is_empty());
        host.shared.set_admission(vec![key_id(&kg)]);
        let guest = joiner(kg, "Guest", &addr);
        wait(&guest, |e| matches!(e, Event::Host(id) if *id == addr.id));
        wait_peers(&guest, 1);
        wait_peers(&host, 1);
        assert_eq!(peer_ids(&host), vec![key_id(&kg)]);
        // Un-inviting stops new joins; it does not rewrite the running room.
        host.shared.set_admission(vec![]);
        let again = joiner(generate_key(), "Other", &addr);
        wait(
            &again,
            |e| matches!(e, Event::Error(m) if m.contains("not admitted")),
        );
    }
    #[test]
    fn shared_lobby_host_refuses_join_requests() {
        let host = Handle::start([21; 32], "Host".into(), None, true).unwrap();
        let Event::Ready(info) = wait(&host, |e| matches!(e, Event::Ready(_))) else {
            unreachable!()
        };
        let addr: Address = serde_json::from_str(&info).unwrap();
        let guest = joiner(generate_key(), "Guest", &addr);
        wait(
            &guest,
            |e| matches!(e, Event::Error(m) if m.contains("not hosting")),
        );
        assert!(host.shared.peers.read().unwrap().is_empty());
    }
    #[test]
    fn wrong_room_secret_never_admits_peer() {
        let host = Handle::start([8; 32], "Host".into(), None, true).unwrap();
        let Event::Ready(info) = wait(&host, |e| matches!(e, Event::Ready(_))) else {
            unreachable!()
        };
        wait(&host, |e| matches!(e, Event::Host(_)));
        let client = Handle::start(
            [9; 32],
            "Wrong room".into(),
            Some(serde_json::from_str(&info).unwrap()),
            true,
        )
        .unwrap();
        wait(&client, |e| matches!(e, Event::Error(_)));
        assert!(host.shared.peers.read().unwrap().is_empty());
        assert!(client.shared.peers.read().unwrap().is_empty());
    }
    #[test]
    fn proofs_agree_across_connection_and_differ_by_role() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let bind = || {
                Endpoint::builder(presets::Minimal)
                    .alpns(vec![ALPN.to_vec()])
                    .bind()
            };
            let (a, b) = (bind().await.unwrap(), bind().await.unwrap());
            let accepting = b.clone();
            let accepted =
                tokio::spawn(async move { accepting.accept().await.unwrap().await.unwrap() });
            let dialed = a.connect(b.addr(), ALPN).await.unwrap();
            let accepted = accepted.await.unwrap();
            let secret = [11; 32];
            for dialer in [true, false] {
                assert_eq!(
                    proof(&dialed, &secret, AUTH, dialer).unwrap(),
                    proof(&accepted, &secret, AUTH, dialer).unwrap()
                );
            }
            assert_ne!(
                proof(&dialed, &secret, AUTH, true).unwrap(),
                proof(&dialed, &secret, AUTH, false).unwrap()
            );
            assert_ne!(
                proof(&dialed, &secret, AUTH, true).unwrap(),
                proof(&dialed, &[12; 32], AUTH, true).unwrap()
            );
            assert_ne!(
                proof(&dialed, &secret, AUTH, true).unwrap(),
                proof(&dialed, &secret, b"prim-media-auth-v2", true).unwrap()
            );
            a.close().await;
            b.close().await;
        });
    }
    #[test]
    fn echoed_proof_never_admits_dialed_endpoint() {
        // A dialed endpoint without the secret replays the member's own hello.
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let attacker = runtime
            .block_on(
                Endpoint::builder(presets::Minimal)
                    .alpns(vec![ALPN.to_vec()])
                    .bind(),
            )
            .unwrap();
        let address = Address::from_endpoint(&attacker);
        let echo = attacker.clone();
        runtime.spawn(async move {
            let connection = echo.accept().await.unwrap().await.unwrap();
            let (mut send, mut recv) = connection.accept_bi().await.unwrap();
            let mut hello: Hello = read_frame(&mut recv).await.unwrap();
            hello.address = Address::from_endpoint(&echo);
            hello.hosting = true;
            write_frame(&mut send, &hello).await.unwrap();
            connection.closed().await;
        });
        let member = Handle::start([10; 32], "Member".into(), Some(address), true).unwrap();
        wait(
            &member,
            |e| matches!(e, Event::Error(m) if m.contains("room authentication failed")),
        );
        assert!(member.shared.peers.read().unwrap().is_empty());
        assert!(member.shared.host.read().unwrap().is_empty());
        drop(member);
        runtime.block_on(attacker.close());
    }
    #[test]
    fn published_record_has_no_room_secret() {
        let record = Record {
            version: 1,
            address: Address {
                id: "endpoint".into(),
                relay: None,
                direct: vec![],
            },
            expires: 10,
        };
        let json = serde_json::to_value(record).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 3);
        assert!(json.get("secret").is_none());
        assert!(json.get("lobby_code").is_none());
    }
    #[test]
    #[ignore = "uses public Iroh relay and mainline DHT"]
    fn public_dht_discovers_host_without_address_exchange() {
        let nonce = format!("{:?}", std::time::SystemTime::now());
        let secret = *blake3::hash(nonce.as_bytes()).as_bytes();
        let host = Handle::start(secret, "Host".into(), None, false).unwrap();
        let deadline = Instant::now() + Duration::from_secs(100);
        let mut published = false;
        while Instant::now() < deadline {
            if let Ok(Event::Status(status)) = host.events.recv_timeout(Duration::from_secs(1)) {
                if status == "Friends lobby published" {
                    published = true;
                    break;
                }
            }
        }
        assert!(published, "public DHT publication did not complete");
        let client = Handle::start(secret, "Client".into(), None, false).unwrap();
        let deadline = Instant::now() + Duration::from_secs(70);
        while Instant::now() < deadline {
            // Drain the host's state events just as the Godot owner does.
            for _ in host.events.try_iter() {}
            for _ in client.events.try_iter() {}
            if host.shared.peers.read().unwrap().len() == 1
                && client.shared.peers.read().unwrap().len() == 1
            {
                assert_eq!(
                    *client.shared.host.read().unwrap(),
                    *host.shared.local.read().unwrap()
                );
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("DHT client did not join published host");
    }
}
