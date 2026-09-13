//! Room-scoped, read-only media. No payload travels through Godot or voice queues.
mod http;
#[cfg(test)]
mod tests;

use crate::network::Shared;
use anyhow::{bail, ensure, Context, Result};
use bytes::Bytes;
use futures_lite::StreamExt;
use iroh::{endpoint::Connection, Endpoint};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime},
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    sync::{mpsc, Semaphore},
    time::Instant,
};

pub const ALPN: &[u8] = b"prim/media-range/1";
const BLOCK: u64 = 1024 * 1024;
const CACHE_BYTES: usize = 32 * 1024 * 1024;
const OK: u8 = 0;
const GONE: u8 = 1;
const WAIT: u8 = 2;
const DENIED: u8 = 3;
const INVALID: u8 = 4;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub id: String,
    pub owner: String,
    pub name: String,
    #[serde(with = "size_string")]
    pub size: u64,
}
// Godot JSON numbers are doubles. Keep byte sizes lossless through its parser.
mod size_string {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(size: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&size.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}
impl Descriptor {
    pub fn valid(&self) -> bool {
        self.id.len() == 64
            && self.id.bytes().all(|b| b.is_ascii_hexdigit())
            && self.owner.parse::<iroh::EndpointId>().is_ok()
            && !self.name.is_empty()
            && self.name.len() <= 512
            && !self.name.chars().any(char::is_control)
            && self.size < (1u64 << 53)
    }
}
#[derive(Clone, Default, Serialize)]
pub struct Viewer {
    pub peer: String,
    pub name: String,
    pub path: String,
    pub consent: String,
    pub bytes_sent: u64,
}
#[derive(Default, Serialize)]
pub struct View {
    pub descriptor: Option<Descriptor>,
    pub url: String,
    pub status: String,
    pub hosted: bool,
    pub viewers: HashMap<String, Viewer>,
    pub bytes_received: u64,
    pub cache_bytes: usize,
}
enum Command {
    Share(PathBuf, u64),
    Receive(Descriptor, u64),
}

pub struct Media {
    tx: mpsc::Sender<Command>,
    rx: Mutex<Option<mpsc::Receiver<Command>>>,
    epoch: AtomicU64,
    lifecycle: Mutex<()>,
    pub view: Mutex<View>,
    source: Mutex<Option<Arc<Source>>>,
    remote: Mutex<Option<Arc<Remote>>>,
    connections: Mutex<HashMap<String, Connection>>,
    pub upload_mbps: AtomicU64,
    #[cfg(test)]
    test_path: AtomicU8,
    pacer: tokio::sync::Mutex<Instant>,
    cache: tokio::sync::Mutex<Cache>,
}
struct Source {
    descriptor: Descriptor,
    file: tokio::sync::Mutex<tokio::fs::File>,
    modified: Option<SystemTime>,
    active: AtomicBool,
}
struct Remote {
    descriptor: Descriptor,
    connection: Connection,
    active: AtomicBool,
    path: String,
}
#[derive(Default)]
struct Cache {
    id: String,
    blocks: HashMap<u64, Bytes>,
    order: VecDeque<u64>,
    bytes: usize,
}
impl Cache {
    fn insert(&mut self, offset: u64, bytes: Bytes) {
        if self.blocks.contains_key(&offset) {
            return;
        }
        while self.bytes + bytes.len() > CACHE_BYTES {
            if let Some(old) = self.order.pop_front() {
                self.bytes -= self.blocks.remove(&old).unwrap().len();
            } else {
                break;
            }
        }
        self.bytes += bytes.len();
        self.order.push_back(offset);
        self.blocks.insert(offset, bytes);
    }
}
impl Media {
    pub fn new() -> Arc<Self> {
        let (tx, rx) = mpsc::channel(8);
        Arc::new(Self {
            tx,
            rx: Mutex::new(Some(rx)),
            epoch: AtomicU64::new(0),
            lifecycle: Mutex::new(()),
            view: Mutex::new(View::default()),
            source: Mutex::new(None),
            remote: Mutex::new(None),
            connections: Mutex::new(HashMap::new()),
            upload_mbps: AtomicU64::new(100),
            #[cfg(test)]
            test_path: AtomicU8::new(255),
            pacer: tokio::sync::Mutex::new(Instant::now()),
            cache: tokio::sync::Mutex::new(Cache::default()),
        })
    }
    pub fn stop(&self) {
        let _lifecycle = self.lifecycle.lock().unwrap();
        self.epoch.fetch_add(1, Ordering::AcqRel);
        if let Some(s) = self.source.lock().unwrap().take() {
            s.active.store(false, Ordering::Release);
        }
        if let Some(r) = self.remote.lock().unwrap().take() {
            r.active.store(false, Ordering::Release);
            r.connection.close(0u8.into(), b"media stopped");
        }
        for (_, c) in self.connections.lock().unwrap().drain() {
            c.close(0u8.into(), b"media stopped");
        }
        *self.view.lock().unwrap() = View::default();
        if let Ok(mut cache) = self.cache.try_lock() {
            *cache = Cache::default();
        }
    }
    pub fn share(&self, path: String) -> bool {
        self.stop();
        self.view.lock().unwrap().status = "Opening shared file…".into();
        self.tx
            .try_send(Command::Share(
                path.into(),
                self.epoch.load(Ordering::Acquire),
            ))
            .is_ok()
    }
    pub fn receive(&self, descriptor: Descriptor) -> bool {
        if !descriptor.valid() {
            return false;
        }
        self.stop();
        self.view.lock().unwrap().status = "Connecting to shared file…".into();
        self.tx
            .try_send(Command::Receive(
                descriptor,
                self.epoch.load(Ordering::Acquire),
            ))
            .is_ok()
    }
    fn effective_path(&self, path: u8) -> u8 {
        #[cfg(test)]
        {
            let forced = self.test_path.load(Ordering::Acquire);
            if forced != 255 {
                return forced;
            }
        }
        path
    }
    pub fn consent(&self, id: &str, peer: &str, allow: bool) {
        let mut view = self.view.lock().unwrap();
        if view.descriptor.as_ref().is_some_and(|d| d.id == id) {
            if let Some(v) = view.viewers.get_mut(peer) {
                v.consent = if allow { "allowed" } else { "denied" }.into();
            }
        }
    }
    fn status(&self, id: &str, status: &str) {
        let mut v = self.view.lock().unwrap();
        if v.descriptor.as_ref().is_some_and(|d| d.id == id) {
            v.status = status.into();
        }
    }
    pub async fn run(
        self: Arc<Self>,
        endpoint: Endpoint,
        state: Arc<Shared>,
        secret: [u8; 32],
    ) -> Result<()> {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let port = listener.local_addr()?.port();
        let service = self.clone();
        let room = state.clone();
        tokio::spawn(async move {
            http::serve(listener, service, room).await;
        });
        let mut rx = self
            .rx
            .lock()
            .unwrap()
            .take()
            .context("media already started")?;
        loop {
            tokio::select! {
                command = rx.recv() => {
                    let Some(command) = command else { break; };
                    let epoch = match &command { Command::Share(_, e) | Command::Receive(_,e) => *e };
                    if epoch != self.epoch.load(Ordering::Acquire) { continue; }
                    let result = match command {
                        Command::Share(path, _) => self.open_file(path, &state, epoch).await,
                        Command::Receive(d, _) => self.open_remote(d, &endpoint, &state, secret, port, epoch).await,
                    };
                    if result.is_err() && epoch == self.epoch.load(Ordering::Acquire) {
                        self.view.lock().unwrap().status = "Shared file unavailable. Choose it again to retry.".into();
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(50)) => {
                    if state.is_stopped() { break; }
                    if self.remote.lock().unwrap().is_none() {
                        if let Ok(mut cache) = self.cache.try_lock() { *cache = Cache::default(); }
                    }
                }
            }
        }
        self.stop();
        Ok(())
    }
    async fn open_file(&self, path: PathBuf, state: &Shared, epoch: u64) -> Result<()> {
        ensure!(state.is_host(), "only the host shares files");
        let file = tokio::fs::File::open(&path).await?;
        let meta = file.metadata().await?;
        ensure!(meta.is_file(), "not a regular file");
        let name = path
            .file_name()
            .context("missing filename")?
            .to_string_lossy()
            .chars()
            .filter(|c| !c.is_control())
            .take(120)
            .collect();
        let descriptor = Descriptor {
            id: random_id(),
            owner: state.local.read().unwrap().clone(),
            name,
            size: meta.len(),
        };
        ensure!(descriptor.valid(), "unsupported file metadata");
        let _lifecycle = self.lifecycle.lock().unwrap();
        ensure!(
            epoch == self.epoch.load(Ordering::Acquire) && !state.is_stopped(),
            "cancelled"
        );
        let source = Arc::new(Source {
            descriptor: descriptor.clone(),
            file: tokio::sync::Mutex::new(file),
            modified: meta.modified().ok(),
            active: AtomicBool::new(true),
        });
        *self.source.lock().unwrap() = Some(source);
        *self.view.lock().unwrap() = View {
            descriptor: Some(descriptor),
            hosted: true,
            status: "Sharing file".into(),
            ..View::default()
        };
        Ok(())
    }
    async fn open_remote(
        &self,
        descriptor: Descriptor,
        endpoint: &Endpoint,
        state: &Shared,
        secret: [u8; 32],
        port: u16,
        epoch: u64,
    ) -> Result<()> {
        ensure!(
            descriptor.owner == *state.host.read().unwrap(),
            "not the room host"
        );
        let addr = state
            .media_peer(&descriptor.owner)
            .context("host not connected")?
            .0
            .parse()?;
        let connection =
            tokio::time::timeout(Duration::from_secs(10), endpoint.connect(addr, ALPN)).await??;
        let result = tokio::time::timeout(Duration::from_secs(8), async {
            let (mut send, mut recv) = connection.open_bi().await?;
            send.write_all(&auth(&connection, &secret)?).await?;
            send.finish()?;
            let mut reply = [0; 32];
            recv.read_exact(&mut reply).await?;
            ensure!(
                reply == auth(&connection, &secret)?,
                "media authentication failed"
            );
            anyhow::Ok(())
        })
        .await;
        let _lifecycle = self.lifecycle.lock().unwrap();
        if !matches!(result, Ok(Ok(()))) || epoch != self.epoch.load(Ordering::Acquire) {
            connection.close(1u8.into(), b"media cancelled");
            bail!("media handshake failed");
        }
        let path = format!("/{}/{}", random_id(), descriptor.id);
        let remote = Arc::new(Remote {
            descriptor: descriptor.clone(),
            connection,
            active: AtomicBool::new(true),
            path: path.clone(),
        });
        *self.remote.lock().unwrap() = Some(remote);
        *self.view.lock().unwrap() = View {
            descriptor: Some(descriptor),
            url: format!("http://127.0.0.1:{port}{path}"),
            status: "Waiting for shared video…".into(),
            ..View::default()
        };
        Ok(())
    }
    async fn pace(&self, length: usize) {
        let mbps = self.upload_mbps.load(Ordering::Acquire).clamp(1, 1000);
        let wait = {
            let mut next = self.pacer.lock().await;
            let wait = (*next).max(Instant::now());
            *next = wait + Duration::from_secs_f64(length as f64 / (mbps as f64 * 125_000.0));
            wait
        };
        tokio::time::sleep_until(wait).await;
    }
}
fn random_id() -> String {
    iroh::SecretKey::generate().public().to_string()
}
fn auth(c: &Connection, secret: &[u8; 32]) -> Result<[u8; 32]> {
    let mut material = [0; 32];
    c.export_keying_material(&mut material, b"prim-media-auth-v1", b"")
        .map_err(|_| anyhow::anyhow!("key export failed"))?;
    Ok(*blake3::keyed_hash(secret, &material).as_bytes())
}
// 0 unknown, 1 selected direct, 2 selected relay. Presence alone is insufficient.
fn selected_path(c: &Connection) -> u8 {
    c.paths().iter().find(|p| p.is_selected()).map_or(0, |p| {
        if p.is_ip() {
            1
        } else if p.is_relay() {
            2
        } else {
            0
        }
    })
}
fn gate(path: u8, consent: &str) -> u8 {
    if path == 1 || (path == 2 && consent == "allowed") {
        OK
    } else if path == 2 && consent == "denied" {
        DENIED
    } else {
        WAIT
    }
}
pub async fn accept(connection: Connection, state: Arc<Shared>, secret: [u8; 32]) -> Result<()> {
    let peer = connection.remote_id().to_string();
    ensure!(
        state.is_host() && state.media_peer(&peer).is_some(),
        "not a room viewer"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        let (mut send, mut recv) = connection.accept_bi().await?;
        let mut proof = [0; 32];
        recv.read_exact(&mut proof).await?;
        ensure!(proof == auth(&connection, &secret)?, "wrong room");
        send.write_all(&proof).await?;
        send.finish()?;
        anyhow::Ok(())
    })
    .await??;
    {
        let mut connections = state.media.connections.lock().unwrap();
        if connections.contains_key(&peer) {
            connection.close(2u8.into(), b"duplicate media connection");
            return Ok(());
        }
        connections.insert(peer.clone(), connection.clone());
    }
    let path = Arc::new(AtomicU8::new(selected_path(&connection)));
    let watcher_connection = connection.clone();
    let watcher_path = path.clone();
    let watcher = tokio::spawn(async move {
        let mut stream = watcher_connection.paths_stream();
        while let Some(paths) = stream.next().await {
            let value = paths.iter().find(|p| p.is_selected()).map_or(0, |p| {
                if p.is_ip() {
                    1
                } else if p.is_relay() {
                    2
                } else {
                    0
                }
            });
            watcher_path.store(value, Ordering::Release);
        }
        watcher_path.store(0, Ordering::Release);
    });
    let permits = Arc::new(Semaphore::new(4));
    let connected = Instant::now();
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            streams = connection.accept_bi() => {
                let Ok((mut send, mut recv)) = streams else { break; };
                let Ok(permit) = permits.clone().try_acquire_owned() else { let _ = send.reset(5u8.into()); let _ = recv.stop(5u8.into()); continue; };
                let s = state.clone(); let p = peer.clone(); let path = path.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    let _ = tokio::time::timeout(Duration::from_secs(15), serve_range(&mut send, &mut recv, s, &p, path, connected)).await;
                });
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                if state.is_stopped() || state.media_peer(&peer).is_none() || !state.is_host() { break; }
                update_viewer(&state, &peer, path.load(Ordering::Acquire), connected);
            }
            _ = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }
    tasks.abort_all();
    watcher.abort();
    connection.close(0u8.into(), b"media disconnected");
    let mut connections = state.media.connections.lock().unwrap();
    if connections
        .get(&peer)
        .is_some_and(|c| c.stable_id() == connection.stable_id())
    {
        connections.remove(&peer);
        state.media.view.lock().unwrap().viewers.remove(&peer);
    }
    Ok(())
}
fn update_viewer(state: &Shared, peer: &str, path: u8, connected: Instant) {
    let path = state.media.effective_path(path);
    let name = state.media_peer(peer).map(|p| p.1).unwrap_or_default();
    let mut view = state.media.view.lock().unwrap();
    if !view.hosted {
        return;
    }
    let v = view.viewers.entry(peer.into()).or_insert_with(|| Viewer {
        peer: peer.into(),
        name,
        consent: "pending".into(),
        ..Viewer::default()
    });
    v.path = if path == 1 {
        "direct"
    } else if path == 2 && connected.elapsed() >= Duration::from_secs(5) {
        "relay"
    } else {
        "connecting"
    }
    .into();
}
async fn serve_range(
    send: &mut iroh::endpoint::SendStream,
    recv: &mut iroh::endpoint::RecvStream,
    state: Arc<Shared>,
    peer: &str,
    path: Arc<AtomicU8>,
    connected: Instant,
) -> Result<()> {
    // Fixed-size wire header bounds allocations before reading untrusted input.
    let mut header = [0u8; 80];
    recv.read_exact(&mut header).await?;
    let id = std::str::from_utf8(&header[..64])?;
    let offset = u64::from_be_bytes(header[64..72].try_into().unwrap());
    let length = u64::from_be_bytes(header[72..80].try_into().unwrap());
    let source = state.media.source.lock().unwrap().clone();
    let Some(source) = source.filter(|s| s.descriptor.id == id && s.active.load(Ordering::Acquire))
    else {
        send.write_all(&[GONE]).await?;
        send.finish()?;
        return Ok(());
    };
    if length == 0
        || length > BLOCK
        || offset
            .checked_add(length)
            .is_none_or(|end| end > source.descriptor.size)
    {
        send.write_all(&[INVALID]).await?;
        send.finish()?;
        return Ok(());
    }
    update_viewer(&state, peer, path.load(Ordering::Acquire), connected);
    let decision = || {
        if !source.active.load(Ordering::Acquire)
            || state.is_stopped()
            || state.media_peer(peer).is_none()
        {
            return GONE;
        }
        let v = state.media.view.lock().unwrap();
        let consent = v
            .viewers
            .get(peer)
            .map(|v| v.consent.as_str())
            .unwrap_or("pending");
        gate(
            state.media.effective_path(path.load(Ordering::Acquire)),
            consent,
        )
    };
    let code = decision();
    if code != OK {
        send.write_all(&[code]).await?;
        send.finish()?;
        return Ok(());
    }
    let bytes = {
        let mut file = source.file.lock().await;
        let meta = file.metadata().await?;
        if meta.len() != source.descriptor.size || meta.modified().ok() != source.modified {
            source.active.store(false, Ordering::Release);
            state
                .media
                .status(id, "Shared file changed. Share it again.");
            send.write_all(&[GONE]).await?;
            send.finish()?;
            return Ok(());
        }
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        let mut bytes = vec![0; length as usize];
        file.read_exact(&mut bytes).await?;
        bytes
    };
    send.write_all(&[OK]).await?;
    for chunk in bytes.chunks(65536) {
        state.media.pace(chunk.len()).await;
        ensure!(decision() == OK, "media permission changed");
        send.write_all(chunk).await?;
        if let Some(v) = state.media.view.lock().unwrap().viewers.get_mut(peer) {
            v.bytes_sent += chunk.len() as u64;
        }
    }
    send.finish()?;
    Ok(())
}
impl Remote {
    async fn block(&self, offset: u64, media: &Media, room: &Shared) -> Result<Bytes> {
        ensure!(
            self.active.load(Ordering::Acquire) && !room.is_stopped(),
            "cancelled"
        );
        let mut cache = media.cache.lock().await;
        ensure!(self.active.load(Ordering::Acquire), "cancelled");
        if cache.id != self.descriptor.id {
            *cache = Cache {
                id: self.descriptor.id.clone(),
                ..Cache::default()
            };
        }
        if let Some(bytes) = cache.blocks.get(&offset) {
            return Ok(bytes.clone());
        }
        let length = BLOCK.min(self.descriptor.size.saturating_sub(offset));
        ensure!(length > 0, "out of bounds");
        loop {
            ensure!(
                self.active.load(Ordering::Acquire) && !room.is_stopped(),
                "cancelled"
            );
            let attempt = tokio::time::timeout(Duration::from_secs(20), async {
                let (mut send, mut recv) = self.connection.open_bi().await?;
                let mut request = self.descriptor.id.as_bytes().to_vec();
                request.extend(offset.to_be_bytes());
                request.extend(length.to_be_bytes());
                send.write_all(&request).await?;
                send.finish()?;
                let mut code = [0];
                recv.read_exact(&mut code).await?;
                if code[0] != OK {
                    return Ok((code[0], Vec::new()));
                }
                let mut data = vec![0; length as usize];
                recv.read_exact(&mut data).await?;
                anyhow::Ok((OK, data))
            })
            .await;
            match attempt {
                Ok(Ok((OK, data))) => {
                    ensure!(self.active.load(Ordering::Acquire), "cancelled");
                    let data = Bytes::from(data);
                    cache.insert(offset, data.clone());
                    let mut view = media.view.lock().unwrap();
                    if view.descriptor.as_ref() == Some(&self.descriptor) {
                        view.bytes_received += length;
                        view.cache_bytes = cache.bytes;
                        view.status = "Receiving shared file".into();
                    }
                    return Ok(data);
                }
                Ok(Ok((WAIT, _))) => media.status(
                    &self.descriptor.id,
                    "Waiting for a direct path or host relay approval…",
                ),
                Ok(Ok((DENIED, _))) => media.status(
                    &self.descriptor.id,
                    "Host allows direct viewers only. Waiting for a direct path…",
                ),
                Ok(Ok((GONE | INVALID, _))) => {
                    media.status(
                        &self.descriptor.id,
                        "Host stopped sharing or the file changed.",
                    );
                    bail!("source unavailable");
                }
                _ => {
                    if self.connection.close_reason().is_some() {
                        media.status(&self.descriptor.id, "Host stopped sharing.");
                        bail!("disconnected");
                    }
                    media.status(&self.descriptor.id, "Reconnecting to shared video…");
                }
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }
}
