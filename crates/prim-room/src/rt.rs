//! The daemon's realtime voice lane (docs/room/VOICE.md).
//!
//! A `SOCK_SEQPACKET` socket (`rt.sock`) served from its own OS thread with its own
//! current-thread runtime, so nothing the room driver does (snapshots, state floods,
//! a busy core) can delay it. Datagram kinds 2 and 3 never enter the room core: the
//! driver's per-connection receive task stamps them with CLOCK_MONOTONIC and hands them
//! here through a bounded channel; this lane checks the sender against a membership
//! view the driver publishes, decodes strictly and queues them for the voice lease
//! holder. In the other direction a `VoiceOut` goes straight into the established
//! member connections' datagram queues.
//!
//! Everything on the lane thread is single-threaded (`Rc<RefCell<..>>`); no borrow is
//! held across an await.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use room_core::api::{ConnId, LocalCommand, LocalEvent, Update};
use room_core::frame::{datagram_kind, dgram_kind, Datagram};
use room_core::ids::Key;
use room_core::rt::{peer_flags, refuse, revoke, Role, RtMsg, RT_MAX_MESSAGE, RT_PROTO};
use room_core::staterec::{Class, Lifetime, Scope};
use room_core::voice::{
    voice_state, Fence, IncarnationFence, VoiceEnvelope, VoiceState, VOICE_KEY,
};
use serde::{Deserialize, Serialize};
use tokio::io::unix::AsyncFd;
use tokio::sync::{mpsc, Notify};
use tokio::time::Instant;

use crate::driver::{DgramQueue, DriverHandle};
use crate::media_age::{MediaAge, Verdict};

/// Lease time-to-live: any message renews it.
pub const LEASE_TTL_MS: u32 = 2000;
/// Messages queued for the voice holder.
pub const IN_QUEUE_MAX: usize = 256;
pub const IN_PER_SENDER_MAX: usize = 32;
/// A `VoiceIn` older than this (since `received_at`) is dropped at dequeue.
pub const IN_MAX_AGE_US: u64 = 100_000;
/// `max_age_ms` of an outbound voice datagram in the per-peer driver queue.
pub const OUT_MAX_AGE_MS: u32 = 40;
const SOCK_BUF: libc::c_int = 64 * 1024;
const INGRESS_CAP: usize = 1024;
const HELLO_TIMEOUT: Duration = Duration::from_secs(2);
const CLOSE_GRACE: Duration = Duration::from_millis(200);
const RESERVOIR: usize = 32768;

// ---- clock ------------------------------------------------------------------------

/// CLOCK_MONOTONIC in microseconds: the one clock domain shared by the daemon and its
/// local clients.
pub fn mono_us() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid out-pointer; CLOCK_MONOTONIC always exists on Linux.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000 + ts.tv_nsec as u64 / 1000
}

// ---- SOCK_SEQPACKET ----------------------------------------------------------------

fn sockaddr_un(path: &Path) -> io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: sockaddr_un is plain old data; all-zero is a valid value.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.len() >= addr.sun_path.len() || bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket path too long",
        ));
    }
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (d, s) in addr.sun_path.iter_mut().zip(bytes) {
        *d = *s as libc::c_char;
    }
    Ok((
        addr,
        std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
    ))
}

fn new_seqpacket() -> io::Result<OwnedFd> {
    // SAFETY: plain syscall; the fd is wrapped immediately.
    let fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a fresh, owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn set_bufs(fd: RawFd) {
    for opt in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
        // SAFETY: valid fd, pointer to a c_int of the stated size.
        unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                opt,
                &SOCK_BUF as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
    }
}

fn raw_recv(fd: RawFd, buf: &mut [u8]) -> io::Result<usize> {
    // SAFETY: `buf` is valid for `buf.len()` bytes.
    let n = unsafe {
        libc::recv(
            fd,
            buf.as_mut_ptr() as *mut _,
            buf.len(),
            libc::MSG_DONTWAIT,
        )
    };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

fn raw_send(fd: RawFd, data: &[u8]) -> io::Result<()> {
    // SAFETY: `data` is valid for `data.len()` bytes.
    let n = unsafe {
        libc::send(
            fd,
            data.as_ptr() as *const _,
            data.len(),
            libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
        )
    };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// One connected `SOCK_SEQPACKET` endpoint: one message per packet.
pub struct SeqSocket {
    fd: AsyncFd<OwnedFd>,
}

impl SeqSocket {
    fn from_fd(fd: OwnedFd) -> io::Result<SeqSocket> {
        set_bufs(fd.as_raw_fd());
        Ok(SeqSocket {
            fd: AsyncFd::new(fd)?,
        })
    }

    /// Connect to a daemon's `rt.sock` (clients and tests).
    pub fn connect(path: &Path) -> io::Result<SeqSocket> {
        let fd = new_seqpacket()?;
        let (addr, len) = sockaddr_un(path)?;
        // SAFETY: `addr` is a valid sockaddr_un of length `len`.
        let r = unsafe { libc::connect(fd.as_raw_fd(), &addr as *const _ as *const _, len) };
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        SeqSocket::from_fd(fd)
    }

    /// The next message; `None` once the peer closed.
    pub async fn recv_raw(&self) -> io::Result<Option<Vec<u8>>> {
        let mut buf = vec![0u8; RT_MAX_MESSAGE + 1];
        loop {
            let mut g = self.fd.readable().await?;
            match g.try_io(|f| raw_recv(f.as_raw_fd(), &mut buf)) {
                Ok(Ok(0)) => return Ok(None),
                Ok(Ok(n)) => {
                    buf.truncate(n);
                    return Ok(Some(buf));
                }
                Ok(Err(e)) if e.kind() == io::ErrorKind::ConnectionReset => return Ok(None),
                Ok(Err(e)) => return Err(e),
                Err(_would_block) => continue,
            }
        }
    }

    /// The next decoded message. A message that fails to decode is an `InvalidData` error.
    pub async fn recv_msg(&self) -> io::Result<Option<RtMsg>> {
        match self.recv_raw().await? {
            None => Ok(None),
            Some(b) => RtMsg::decode(&b)
                .map(Some)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string())),
        }
    }

    /// Send one message, waiting while the socket buffer is full.
    pub async fn send_msg(&self, m: &RtMsg) -> io::Result<()> {
        let b = m.encode();
        loop {
            let mut g = self.fd.writable().await?;
            match g.try_io(|f| raw_send(f.as_raw_fd(), &b)) {
                Ok(r) => return r,
                Err(_would_block) => continue,
            }
        }
    }

    /// Non-blocking send; `Ok(false)` when the buffer is full.
    pub fn try_send_msg(&self, m: &RtMsg) -> io::Result<bool> {
        match raw_send(self.fd.as_raw_fd(), &m.encode()) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(false),
            Err(e) => Err(e),
        }
    }
}

struct SeqListener {
    fd: AsyncFd<OwnedFd>,
}

/// Bind (blocking, before the lane thread starts): create the directory (0700), replace
/// a stale socket file, refuse to steal a live one.
fn bind_listener(path: &Path) -> io::Result<OwnedFd> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    if path.exists() {
        if SeqSocket::connect_blocking_probe(path) {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("a daemon is already listening on {}", path.display()),
            ));
        }
        std::fs::remove_file(path)?;
    }
    let fd = new_seqpacket()?;
    let (addr, len) = sockaddr_un(path)?;
    // SAFETY: valid fd and sockaddr.
    if unsafe { libc::bind(fd.as_raw_fd(), &addr as *const _ as *const _, len) } < 0 {
        return Err(io::Error::last_os_error());
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    // SAFETY: valid fd.
    if unsafe { libc::listen(fd.as_raw_fd(), 16) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

impl SeqSocket {
    fn connect_blocking_probe(path: &Path) -> bool {
        let Ok(fd) = new_seqpacket() else {
            return false;
        };
        let Ok((addr, len)) = sockaddr_un(path) else {
            return false;
        };
        // SAFETY: valid fd and sockaddr.
        unsafe { libc::connect(fd.as_raw_fd(), &addr as *const _ as *const _, len) == 0 }
    }
}

impl SeqListener {
    async fn accept(&self) -> io::Result<SeqSocket> {
        loop {
            let mut g = self.fd.readable().await?;
            let r = g.try_io(|f| {
                // SAFETY: accept4 with null address pointers is allowed.
                let fd = unsafe {
                    libc::accept4(
                        f.as_raw_fd(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                    )
                };
                if fd < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    // SAFETY: fresh descriptor.
                    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
                }
            });
            match r {
                Ok(Ok(fd)) => return SeqSocket::from_fd(fd),
                Ok(Err(e)) => return Err(e),
                Err(_would_block) => continue,
            }
        }
    }
}

// ---- statistics --------------------------------------------------------------------

/// Log2 histogram in microseconds plus a ring of the latest samples for exact
/// percentiles over that window.
#[derive(Clone)]
struct Hist {
    buckets: [u64; 41],
    ring: Vec<u32>,
    pos: usize,
    count: u64,
    max: u64,
}

impl Hist {
    fn new() -> Hist {
        Hist {
            buckets: [0; 41],
            ring: Vec::new(),
            pos: 0,
            count: 0,
            max: 0,
        }
    }

    fn record(&mut self, us: u64) {
        // Bucket 0 is 0 us; bucket i (i >= 1) covers [2^(i-1), 2^i).
        let b = (64 - us.leading_zeros() as usize).min(40);
        self.buckets[b] += 1;
        self.count += 1;
        self.max = self.max.max(us);
        let v = us.min(u32::MAX as u64) as u32;
        if self.ring.len() < RESERVOIR {
            self.ring.push(v);
        } else {
            self.ring[self.pos] = v;
            self.pos = (self.pos + 1) % RESERVOIR;
        }
    }

    fn json(&self) -> HistJson {
        let mut s = self.ring.clone();
        s.sort_unstable();
        let pct = |p: f64| -> u64 {
            if s.is_empty() {
                return 0;
            }
            let idx = ((p * s.len() as f64).ceil() as usize).clamp(1, s.len()) - 1;
            s[idx] as u64
        };
        HistJson {
            count: self.count,
            buckets: self.buckets.to_vec(),
            p50_us: pct(0.5),
            p99_us: pct(0.99),
            p999_us: pct(0.999),
            max_us: self.max,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct HistJson {
    pub count: u64,
    /// Bucket 0 counts 0 us; bucket i counts [2^(i-1), 2^i) us.
    pub buckets: Vec<u64>,
    pub p50_us: u64,
    pub p99_us: u64,
    pub p999_us: u64,
    pub max_us: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct DropsJson {
    pub no_lease: u64,
    pub not_member: u64,
    pub decode: u64,
    pub queue_full: u64,
    pub per_sender: u64,
    pub age: u64,
    /// Dropped at enqueue for arriving over `IN_MAX_AGE_US` later than the sender's
    /// media clock says it should (listener-side pause; see `media_age`).
    #[serde(default)]
    pub age_media: u64,
    /// Dropped at the daemon's per-sender incarnation fence: a retired stream's packet.
    #[serde(default)]
    pub stale_incarnation: u64,
    /// Dropped from the voice client: sent over `OUT_MAX_AGE_MS` before the daemon read it.
    #[serde(default)]
    pub out_age: u64,
    pub wrong_incarnation: u64,
    pub channel_full: u64,
    /// A message from a session that doesn't hold the role it needs.
    pub bad_role: u64,
    /// Kind 3 datagrams (R0 routes none).
    pub viseme: u64,
    /// Queued poses replaced by a newer one (latest value wins).
    pub pose_replaced: u64,
    /// Poses with nowhere to go (no voice holder).
    pub pose_dropped: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct HolderJson {
    pub role: String,
    pub session: u64,
    pub incarnation: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct RtJson {
    pub holders: Vec<HolderJson>,
    /// Messages waiting for the voice holder.
    pub queue_depth: usize,
    /// `VoiceOut` accepted from the voice holder.
    pub sent: u64,
    /// Per-peer datagrams enqueued for those (one `VoiceOut` fans out to every peer).
    pub datagrams_enqueued: u64,
    /// `VoiceIn` written to the voice holder.
    pub received: u64,
    pub drops: DropsJson,
    /// `read_mono_us - VoiceOut.sent_mono_us`: the outbound IPC hop.
    pub out_hop: HistJson,
    /// `write_mono_us - received_at`: time a `VoiceIn` spent in the daemon.
    pub in_queue: HistJson,
    /// `forward_mono_us - PoseOut.source_mono_us`.
    pub pose_hop: HistJson,
}

#[derive(Default)]
struct Counters {
    sent: AtomicU64,
    enqueued: AtomicU64,
    received: AtomicU64,
    no_lease: AtomicU64,
    not_member: AtomicU64,
    decode: AtomicU64,
    queue_full: AtomicU64,
    per_sender: AtomicU64,
    age: AtomicU64,
    age_media: AtomicU64,
    stale_incarnation: AtomicU64,
    out_age: AtomicU64,
    wrong_incarnation: AtomicU64,
    channel_full: AtomicU64,
    bad_role: AtomicU64,
    viseme: AtomicU64,
    pose_replaced: AtomicU64,
    pose_dropped: AtomicU64,
    depth: AtomicUsize,
}

fn bump(c: &AtomicU64) {
    c.fetch_add(1, Ordering::Relaxed);
}

// ---- the shared half (driver <-> lane) -----------------------------------------------

pub(crate) struct ViewEntry {
    pub peer: Key,
    /// The peer advertised `caps::VOICE`.
    pub voice: bool,
    pub dgrams: Arc<DgramQueue>,
    pub cap: usize,
}

struct Ingress {
    conn: ConnId,
    at_us: u64,
    bytes: Vec<u8>,
}

struct Shared {
    view: RwLock<HashMap<ConnId, ViewEntry>>,
    ingress_tx: mpsc::Sender<Ingress>,
    ingress_rx: Mutex<Option<mpsc::Receiver<Ingress>>>,
    view_tx: mpsc::UnboundedSender<()>,
    view_rx: Mutex<Option<mpsc::UnboundedReceiver<()>>>,
    counters: Counters,
    holders: Mutex<Vec<HolderJson>>,
    out_hop: Mutex<Hist>,
    in_queue: Mutex<Hist>,
    pose_hop: Mutex<Hist>,
    clock_offset_us: i64,
}

/// What the driver and the lane share: the membership view, the ingress channel and the
/// statistics. Cloning is cheap.
#[derive(Clone)]
pub struct RtShared(Arc<Shared>);

impl std::fmt::Debug for RtShared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RtShared")
    }
}

impl RtShared {
    /// `clock_offset_us` is added to every `received_at_mono_us` handed to the client
    /// (the mismatched-clock fault hook).
    pub fn new(clock_offset_us: i64) -> RtShared {
        let (ingress_tx, ingress_rx) = mpsc::channel(INGRESS_CAP);
        let (view_tx, view_rx) = mpsc::unbounded_channel();
        RtShared(Arc::new(Shared {
            view: RwLock::new(HashMap::new()),
            ingress_tx,
            ingress_rx: Mutex::new(Some(ingress_rx)),
            view_tx,
            view_rx: Mutex::new(Some(view_rx)),
            counters: Counters::default(),
            holders: Mutex::new(Vec::new()),
            out_hop: Mutex::new(Hist::new()),
            in_queue: Mutex::new(Hist::new()),
            pose_hop: Mutex::new(Hist::new()),
            clock_offset_us,
        }))
    }

    /// Driver side: is this datagram for the lane? If so it is stamped and handed over
    /// (or counted as dropped) and the caller must not pass it on.
    pub fn offer(&self, conn: ConnId, bytes: Vec<u8>) -> Option<Vec<u8>> {
        match datagram_kind(&bytes) {
            Some(dgram_kind::VOICE) | Some(dgram_kind::VISEME) => {
                let at_us = mono_us();
                if self
                    .0
                    .ingress_tx
                    .try_send(Ingress { conn, at_us, bytes })
                    .is_err()
                {
                    bump(&self.0.counters.channel_full);
                }
                None
            }
            _ => Some(bytes),
        }
    }

    pub(crate) fn established(&self, conn: ConnId, e: ViewEntry) {
        self.0.view.write().unwrap().insert(conn, e);
        let _ = self.0.view_tx.send(());
    }

    pub(crate) fn unestablished(&self, conn: ConnId) {
        if self.0.view.write().unwrap().remove(&conn).is_some() {
            let _ = self.0.view_tx.send(());
        }
    }

    /// Test hook: publish a fake established member connection (no transport behind it;
    /// its outbound datagram queue is private) so tests can feed the fast path from many
    /// senders.
    #[doc(hidden)]
    pub fn fake_member(&self, conn: ConnId, peer: Key) {
        self.established(
            conn,
            ViewEntry {
                peer,
                voice: true,
                dgrams: Arc::new(DgramQueue::default()),
                cap: 128,
            },
        );
    }

    pub fn snapshot(&self) -> RtJson {
        let s = &self.0;
        let c = &s.counters;
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        RtJson {
            holders: s.holders.lock().unwrap().clone(),
            queue_depth: c.depth.load(Ordering::Relaxed),
            sent: g(&c.sent),
            datagrams_enqueued: g(&c.enqueued),
            received: g(&c.received),
            drops: DropsJson {
                no_lease: g(&c.no_lease),
                not_member: g(&c.not_member),
                decode: g(&c.decode),
                queue_full: g(&c.queue_full),
                per_sender: g(&c.per_sender),
                age: g(&c.age),
                age_media: g(&c.age_media),
                stale_incarnation: g(&c.stale_incarnation),
                out_age: g(&c.out_age),
                wrong_incarnation: g(&c.wrong_incarnation),
                channel_full: g(&c.channel_full),
                bad_role: g(&c.bad_role),
                viseme: g(&c.viseme),
                pose_replaced: g(&c.pose_replaced),
                pose_dropped: g(&c.pose_dropped),
            },
            // Copy under the lock (a memcpy), sort outside it: the rt thread takes these
            // per packet, and a sort of the whole reservoir would stall it.
            out_hop: s.out_hop.lock().unwrap().clone().json(),
            in_queue: s.in_queue.lock().unwrap().clone().json(),
            pose_hop: s.pose_hop.lock().unwrap().clone().json(),
        }
    }
}

// ---- the lane --------------------------------------------------------------------------

pub struct RtOptions {
    pub socket: PathBuf,
    /// This daemon's member key (the `self_key` of `Welcome`).
    pub self_key: Key,
    pub lease_ttl_ms: u32,
}

impl RtOptions {
    pub fn new(socket: PathBuf, self_key: Key) -> RtOptions {
        RtOptions {
            socket,
            self_key,
            lease_ttl_ms: LEASE_TTL_MS,
        }
    }
}

/// A running lane: its thread and socket.
pub struct RtLane {
    pub socket: PathBuf,
    stop: Arc<Notify>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl RtLane {
    /// Stop the thread, wait for it and remove the socket file.
    pub fn stop(&mut self) {
        self.stop.notify_one();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

impl Drop for RtLane {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Bind `opts.socket` and start the lane thread.
pub fn start(shared: RtShared, driver: DriverHandle, opts: RtOptions) -> io::Result<RtLane> {
    let listener_fd = bind_listener(&opts.socket)?;
    let ingress_rx = shared
        .0
        .ingress_rx
        .lock()
        .unwrap()
        .take()
        .ok_or_else(|| io::Error::other("rt lane already started"))?;
    let view_rx = shared.0.view_rx.lock().unwrap().take().unwrap();
    let stop = Arc::new(Notify::new());
    let stop2 = stop.clone();
    let socket = opts.socket.clone();
    let thread = std::thread::Builder::new()
        .name("prim-rt".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("rt runtime");
            let local = tokio::task::LocalSet::new();
            local.block_on(
                &rt,
                lane_main(
                    shared,
                    driver,
                    opts,
                    listener_fd,
                    ingress_rx,
                    view_rx,
                    stop2,
                ),
            );
        })?;
    Ok(RtLane {
        socket,
        stop,
        thread: Some(thread),
    })
}

struct PeerEntry {
    connected: bool,
    vstate: Option<VoiceState>,
}

struct Core {
    shared: Arc<Shared>,
    self_key: Key,
    ttl: Duration,
    next_session: u64,
    voice: Option<Rc<Session>>,
    tracking: Option<Rc<Session>>,
    peers: BTreeMap<Key, PeerEntry>,
    driver: DriverHandle,
    me: u64,
    next_req: u64,
    last_written: Option<VoiceState>,
    /// The voice-key value the lane last tried to write, for retry after a refusal or
    /// a room reset.
    want_voice: Option<VoiceState>,
    /// The request id of that write.
    voice_req: Option<u64>,
    media_age: HashMap<Key, MediaAge>,
    /// Per-sender incarnation fence, ahead of `media_age`.
    fences: HashMap<Key, IncarnationFence>,
}

type CoreRef = Rc<RefCell<Core>>;

async fn lane_main(
    shared: RtShared,
    driver: DriverHandle,
    opts: RtOptions,
    listener_fd: OwnedFd,
    mut ingress_rx: mpsc::Receiver<Ingress>,
    mut view_rx: mpsc::UnboundedReceiver<()>,
    stop: Arc<Notify>,
) {
    let listener = match AsyncFd::new(listener_fd) {
        Ok(fd) => SeqListener { fd },
        Err(e) => {
            tracing::error!("rt listener: {e}");
            return;
        }
    };
    let (me, events) = driver.attach();
    driver.local(
        me,
        LocalCommand::Subscribe {
            req: 1,
            sub: 1,
            pattern: format!("member/*/{VOICE_KEY}"),
        },
    );
    let core: CoreRef = Rc::new(RefCell::new(Core {
        shared: shared.0.clone(),
        self_key: opts.self_key,
        ttl: Duration::from_millis(opts.lease_ttl_ms as u64),
        next_session: 1,
        voice: None,
        tracking: None,
        peers: BTreeMap::new(),
        driver: driver.clone(),
        me,
        next_req: 2,
        last_written: None,
        want_voice: None,
        voice_req: None,
        media_age: HashMap::new(),
        fences: HashMap::new(),
    }));
    tokio::task::spawn_local(client_events(core.clone(), events));
    core.borrow_mut().refresh_peers();
    loop {
        tokio::select! {
            biased;
            _ = stop.notified() => break,
            ing = ingress_rx.recv() => match ing {
                Some(ing) => core.borrow_mut().on_ingress(ing),
                None => break,
            },
            v = view_rx.recv() => {
                if v.is_none() { break; }
                while view_rx.try_recv().is_ok() {}
                core.borrow_mut().refresh_peers();
            }
            conn = listener.accept() => match conn {
                Ok(sock) => { tokio::task::spawn_local(session_task(core.clone(), sock)); }
                Err(e) => {
                    tracing::error!("rt accept: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
        }
    }
    driver.detach(me);
}

/// The lane's own local-API client: reads `member/*/voice`, so the lane knows who is
/// listening, and drains replies.
async fn client_events(core: CoreRef, mut events: mpsc::Receiver<crate::driver::ClientMsg>) {
    let (driver, me, self_key) = {
        let c = core.borrow();
        (c.driver.clone(), c.me, c.self_key)
    };
    while let Some(msg) = events.recv().await {
        match msg.event {
            LocalEvent::Ready { sub } => loop {
                let updates = driver.drain(me, sub, 256).await;
                let n = updates.len();
                {
                    let mut c = core.borrow_mut();
                    for u in updates {
                        c.apply_update(u, &self_key);
                    }
                    c.refresh_peers();
                }
                if n < 256 {
                    break;
                }
            },
            LocalEvent::Reply {
                req,
                reply: room_core::api::Reply::Error(e),
            } => {
                tracing::debug!("rt lane write refused: {e:?}");
                // Forget the dedup so the next view or room change retries the write.
                let mut c = core.borrow_mut();
                if c.voice_req == Some(req) {
                    c.last_written = None;
                }
            }
            _ => {}
        }
    }
}

fn path_key(path: &str) -> Option<Key> {
    let mut it = path.split('/');
    (it.next()? == "member").then_some(())?;
    let k = Key::from_hex(it.next()?)?;
    (it.next()? == VOICE_KEY && it.next().is_none()).then_some(k)
}

impl Core {
    fn apply_update(&mut self, u: Update, self_key: &Key) {
        match u {
            Update::Reset => {
                for p in self.peers.values_mut() {
                    p.vstate = None;
                }
                // The room closed or reopened: the presence-bound key may be gone or
                // stamped with the old generation. Write it again.
                self.last_written = None;
                self.retry_voice_key();
            }
            Update::Value { writer, value, .. } => {
                if writer == *self_key {
                    return;
                }
                if let Ok(v) = VoiceState::decode(&value) {
                    self.peers
                        .entry(writer)
                        .or_insert(PeerEntry {
                            connected: false,
                            vstate: None,
                        })
                        .vstate = Some(v);
                }
            }
            Update::Removed { path } => {
                if let Some(k) = path_key(&path) {
                    if let Some(p) = self.peers.get_mut(&k) {
                        p.vstate = None;
                    }
                }
            }
            Update::SnapshotDone => {}
        }
    }

    /// Recompute who is connected (from the driver's view), then queue a `Peer` for every
    /// member whose flags changed since the voice holder last heard of it.
    fn refresh_peers(&mut self) {
        self.retry_voice_key();
        {
            let view = self.shared.view.read().unwrap();
            for p in self.peers.values_mut() {
                p.connected = false;
            }
            for e in view.values() {
                let p = self.peers.entry(e.peer).or_insert(PeerEntry {
                    connected: false,
                    vstate: None,
                });
                p.connected |= e.voice;
            }
        }
        let Some(v) = self.voice.clone() else { return };
        let mut out = v.out.borrow_mut();
        let mut any = false;
        for (k, p) in &self.peers {
            let t = peer_tuple(p);
            if out.peers_last.get(k) != Some(&t) {
                out.peers_last.insert(*k, t);
                out.peers.insert(*k, t);
                any = true;
            }
        }
        drop(out);
        if any {
            v.wake.notify_one();
        }
    }

    fn listening(&self, peer: &Key) -> bool {
        match self.peers.get(peer).and_then(|p| p.vstate) {
            Some(s) => s.flags & voice_state::LISTENING != 0,
            None => true, // unknown: send
        }
    }

    fn publish_holders(&self) {
        let mut h = Vec::new();
        for (name, s) in [("voice", &self.voice), ("tracking_source", &self.tracking)] {
            if let Some(s) = s {
                h.push(HolderJson {
                    role: name.into(),
                    session: s.id,
                    incarnation: s.incarnation.get(),
                });
            }
        }
        *self.shared.holders.lock().unwrap() = h;
    }

    /// Write the wanted voice key again if the last write was refused or the room reset.
    fn retry_voice_key(&mut self) {
        if self.last_written.is_none() {
            if let Some(w) = self.want_voice {
                self.write_voice_key(w);
            }
        }
    }

    /// Write `member/<me>/voice` from the voice lease (`None` = lease ended).
    fn write_voice_key(&mut self, state: VoiceState) {
        self.want_voice = Some(state);
        if self.last_written == Some(state) {
            return;
        }
        self.last_written = Some(state);
        let req = self.next_req;
        self.next_req += 1;
        self.voice_req = Some(req);
        self.driver.local(
            self.me,
            LocalCommand::Write {
                req,
                scope: Scope::Member,
                name: VOICE_KEY.into(),
                class: Class::Durable,
                lifetime: Lifetime::PresenceBound,
                value: state.encode(),
            },
        );
    }

    fn voice_state_of(s: &Session) -> VoiceState {
        VoiceState {
            flags: s.flags.get(),
            incarnation: s.incarnation.get(),
        }
    }

    /// Drop every queued outbound datagram of a voice stream other than `keep`.
    fn fence_outbound(&self, keep: u32) {
        for e in self.shared.view.read().unwrap().values() {
            e.dgrams.purge_tagged_except(keep);
        }
    }

    fn acquire(
        &mut self,
        role: Role,
        takeover: bool,
        incarnation: u32,
        flags: u8,
    ) -> Result<Rc<Session>, u8> {
        let slot = match role {
            Role::Voice => &mut self.voice,
            Role::TrackingSource => &mut self.tracking,
        };
        if let Some(old) = slot.take() {
            if !takeover {
                *slot = Some(old);
                return Err(refuse::BUSY);
            }
            old.revoke(revoke::TAKEOVER);
        }
        let s = Rc::new(Session::new(self.next_session, role, incarnation, flags));
        self.next_session += 1;
        s.out.borrow_mut().ctl.push_back(RtMsg::Welcome {
            proto: RT_PROTO,
            self_key: self.self_key,
            lease_ttl_ms: self.ttl.as_millis() as u32,
            session: s.id,
        });
        match role {
            Role::Voice => {
                self.voice = Some(s.clone());
                self.shared.counters.depth.store(0, Ordering::Relaxed);
                // Session fencing: nothing of an older stream survives.
                self.fence_outbound(incarnation);
                self.write_voice_key(Self::voice_state_of(&s));
                self.refresh_peers();
            }
            Role::TrackingSource => self.tracking = Some(s.clone()),
        }
        self.publish_holders();
        Ok(s)
    }

    /// End a session's lease if it still holds it.
    fn release(&mut self, s: &Rc<Session>) {
        let slot = match s.role {
            Role::Voice => &mut self.voice,
            Role::TrackingSource => &mut self.tracking,
        };
        if slot.as_ref().is_some_and(|h| Rc::ptr_eq(h, s)) {
            *slot = None;
            if s.role == Role::Voice {
                self.shared.counters.depth.store(0, Ordering::Relaxed);
                self.write_voice_key(VoiceState {
                    flags: 0,
                    incarnation: 0,
                });
            }
            self.publish_holders();
        }
    }

    fn is_holder(&self, s: &Rc<Session>) -> bool {
        let slot = match s.role {
            Role::Voice => &self.voice,
            Role::TrackingSource => &self.tracking,
        };
        slot.as_ref().is_some_and(|h| Rc::ptr_eq(h, s))
    }

    fn on_ingress(&mut self, ing: Ingress) {
        let c = &self.shared.counters;
        let peer = match self.shared.view.read().unwrap().get(&ing.conn) {
            Some(e) => e.peer,
            None => {
                bump(&c.not_member);
                return;
            }
        };
        let env = match Datagram::decode(&ing.bytes) {
            Ok(Datagram::Voice(env)) => env,
            Ok(Datagram::Viseme(_)) => {
                bump(&c.viseme);
                return;
            }
            _ => {
                bump(&c.decode);
                return;
            }
        };
        let Some(v) = self.voice.clone() else {
            bump(&c.no_lease);
            return;
        };
        // Fence by incarnation first, so a late packet of a retired stream can't reset
        // the media-time state of the current one.
        match self.fences.entry(peer).or_default().judge(env.incarnation) {
            Fence::Stale => {
                bump(&c.stale_incarnation);
                return;
            }
            Fence::NewStream { .. } => {
                self.media_age.remove(&peer);
            }
            Fence::Current => {}
        }
        // Bound by media time: the read stamp alone can't see a backlog released all at
        // once after this daemon was stopped.
        let verdict = self
            .media_age
            .entry(peer)
            .or_insert_with(|| MediaAge::new(IN_MAX_AGE_US))
            .check(ing.at_us, env.incarnation, env.timestamp);
        if verdict == Verdict::TooLate {
            bump(&c.age_media);
            return;
        }
        let mut out = v.out.borrow_mut();
        if out.voice.iter().filter(|i| i.from == peer).count() >= IN_PER_SENDER_MAX {
            if let Some(pos) = out.voice.iter().position(|i| i.from == peer) {
                out.voice.remove(pos);
            }
            bump(&c.per_sender);
        }
        if out.voice.len() >= IN_QUEUE_MAX {
            out.voice.pop_front();
            bump(&c.queue_full);
        }
        out.voice.push_back(InItem {
            from: peer,
            at_us: ing.at_us,
            env,
        });
        c.depth.store(out.voice.len(), Ordering::Relaxed);
        drop(out);
        v.wake.notify_one();
    }

    fn on_voice_out(&mut self, s: &Rc<Session>, sent_mono_us: u64, env: VoiceEnvelope) {
        let c = &self.shared.counters;
        if !self.is_holder(s) || s.role != Role::Voice {
            bump(&c.bad_role);
            return;
        }
        if env.incarnation != s.incarnation.get() {
            bump(&c.wrong_incarnation);
            return;
        }
        let hop_us = mono_us().saturating_sub(sent_mono_us);
        self.shared.out_hop.lock().unwrap().record(hop_us);
        // The client's socket buffer holds messages while this daemon is stopped; they
        // all arrive at once on resume. Same host, same clock: the age is exact.
        if hop_us > OUT_MAX_AGE_MS as u64 * 1000 {
            bump(&c.out_age);
            return;
        }
        bump(&c.sent);
        let bytes = Datagram::Voice(env).encode();
        let inc = s.incarnation.get();
        let view = self.shared.view.read().unwrap();
        for e in view.values() {
            if !e.voice || !self.listening(&e.peer) {
                continue;
            }
            e.dgrams.push(bytes.clone(), OUT_MAX_AGE_MS, inc, e.cap);
            bump(&c.enqueued);
        }
    }

    fn on_pose(
        &mut self,
        s: &Rc<Session>,
        source_mono_us: u64,
        seq: u32,
        pose: room_core::rt::Pose,
    ) {
        let c = &self.shared.counters;
        if !self.is_holder(s) || s.role != Role::TrackingSource {
            bump(&c.bad_role);
            return;
        }
        let Some(v) = self.voice.clone() else {
            bump(&c.pose_dropped);
            return;
        };
        let mut out = v.out.borrow_mut();
        if out.pose.replace((source_mono_us, seq, pose)).is_some() {
            bump(&c.pose_replaced);
        }
        drop(out);
        v.wake.notify_one();
    }

    fn on_set_state(&mut self, s: &Rc<Session>, flags: u8, incarnation: u32) {
        if !self.is_holder(s) || s.role != Role::Voice {
            bump(&self.shared.counters.bad_role);
            return;
        }
        if flags & !(voice_state::LISTENING | voice_state::MIC) != 0 {
            bump(&self.shared.counters.decode);
            return;
        }
        if flags & voice_state::MIC != 0 && incarnation == 0 {
            bump(&self.shared.counters.decode);
            return;
        }
        let changed = s.incarnation.get() != incarnation;
        s.flags.set(flags);
        s.incarnation.set(incarnation);
        if changed {
            self.fence_outbound(incarnation);
        }
        self.write_voice_key(Self::voice_state_of(s));
        self.publish_holders();
    }
}

fn peer_tuple(p: &PeerEntry) -> (u8, u32) {
    let mut f = 0;
    if p.connected {
        f |= peer_flags::CONNECTED;
    }
    let mut inc = 0;
    if let Some(v) = p.vstate {
        if v.flags & voice_state::LISTENING != 0 {
            f |= peer_flags::LISTENING;
        }
        if v.flags & voice_state::MIC != 0 {
            f |= peer_flags::MIC;
        }
        inc = v.incarnation;
    }
    (f, inc)
}

// ---- sessions ----------------------------------------------------------------------------

struct InItem {
    from: Key,
    at_us: u64,
    env: VoiceEnvelope,
}

#[derive(Default)]
struct Out {
    ctl: VecDeque<RtMsg>,
    peers: BTreeMap<Key, (u8, u32)>,
    /// What the holder was last told per peer (queued or sent).
    peers_last: BTreeMap<Key, (u8, u32)>,
    pose: Option<(u64, u32, room_core::rt::Pose)>,
    voice: VecDeque<InItem>,
}

struct Session {
    id: u64,
    role: Role,
    incarnation: Cell<u32>,
    flags: Cell<u8>,
    last_rx: Cell<Instant>,
    /// Set once the session is ending: the time by which it must be gone.
    close_by: Cell<Option<Instant>>,
    out: RefCell<Out>,
    wake: Notify,
}

/// What `candidate` picked: the bytes, and where to remove it from once sent.
enum Slot {
    Ctl,
    Peer(Key),
    Pose,
    Voice,
}

struct Candidate {
    bytes: Vec<u8>,
    slot: Slot,
    terminal: bool,
    /// For voice: the daemon-side queueing delay at the time of sending.
    stamp_us: Option<u64>,
    pose_source_us: Option<u64>,
}

impl Session {
    fn new(id: u64, role: Role, incarnation: u32, flags: u8) -> Session {
        Session {
            id,
            role,
            incarnation: Cell::new(incarnation),
            flags: Cell::new(flags),
            last_rx: Cell::new(Instant::now()),
            close_by: Cell::new(None),
            out: RefCell::new(Out::default()),
            wake: Notify::new(),
        }
    }

    /// Queue `Revoked{reason}` as the last message and give the session a moment to
    /// flush it.
    fn revoke(&self, reason: u8) {
        if self.close_by.get().is_some() {
            return;
        }
        let mut out = self.out.borrow_mut();
        out.voice.clear();
        out.peers.clear();
        out.pose = None;
        out.ctl.push_back(RtMsg::Revoked { reason });
        drop(out);
        self.close_by.set(Some(Instant::now() + CLOSE_GRACE));
        self.wake.notify_one();
    }

    fn has_pending(&self) -> bool {
        let o = self.out.borrow();
        !(o.ctl.is_empty() && o.peers.is_empty() && o.pose.is_none() && o.voice.is_empty())
    }

    /// The next message to send, without removing it. Expired `VoiceIn`s are dropped
    /// here, so the age bound applies at every attempt.
    fn candidate(&self, shared: &Shared) -> Option<Candidate> {
        let mut out = self.out.borrow_mut();
        if let Some(m) = out.ctl.front() {
            return Some(Candidate {
                bytes: m.encode(),
                terminal: matches!(m, RtMsg::Revoked { .. } | RtMsg::Refused { .. }),
                slot: Slot::Ctl,
                stamp_us: None,
                pose_source_us: None,
            });
        }
        if let Some((k, (flags, inc))) = out.peers.iter().next().map(|(k, v)| (*k, *v)) {
            return Some(Candidate {
                bytes: RtMsg::Peer {
                    key: k,
                    flags,
                    incarnation: inc,
                }
                .encode(),
                terminal: false,
                slot: Slot::Peer(k),
                stamp_us: None,
                pose_source_us: None,
            });
        }
        if let Some((src, seq, pose)) = out.pose {
            return Some(Candidate {
                bytes: RtMsg::Listener {
                    source_mono_us: src,
                    forwarded_mono_us: mono_us(),
                    seq,
                    pose,
                }
                .encode(),
                terminal: false,
                slot: Slot::Pose,
                stamp_us: None,
                pose_source_us: Some(src),
            });
        }
        let now = mono_us();
        while let Some(i) = out.voice.front() {
            if now.saturating_sub(i.at_us) > IN_MAX_AGE_US {
                out.voice.pop_front();
                bump(&shared.counters.age);
                continue;
            }
            shared
                .counters
                .depth
                .store(out.voice.len(), Ordering::Relaxed);
            return Some(Candidate {
                bytes: RtMsg::VoiceIn {
                    from: i.from,
                    received_at_mono_us: i.at_us.wrapping_add_signed(shared.clock_offset_us),
                    env: i.env.clone(),
                }
                .encode(),
                terminal: false,
                slot: Slot::Voice,
                stamp_us: Some(i.at_us),
                pose_source_us: None,
            });
        }
        shared.counters.depth.store(0, Ordering::Relaxed);
        None
    }

    fn commit(&self, c: &Candidate, shared: &Shared) {
        let mut out = self.out.borrow_mut();
        match &c.slot {
            Slot::Ctl => {
                out.ctl.pop_front();
            }
            Slot::Peer(k) => {
                out.peers.remove(k);
            }
            Slot::Pose => {
                out.pose = None;
                if let Some(src) = c.pose_source_us {
                    shared
                        .pose_hop
                        .lock()
                        .unwrap()
                        .record(mono_us().saturating_sub(src));
                }
            }
            Slot::Voice => {
                out.voice.pop_front();
                bump(&shared.counters.received);
                shared
                    .counters
                    .depth
                    .store(out.voice.len(), Ordering::Relaxed);
                if let Some(at) = c.stamp_us {
                    shared
                        .in_queue
                        .lock()
                        .unwrap()
                        .record(mono_us().saturating_sub(at));
                }
            }
        }
    }

    /// Send one queued message. Waits for something to send and for socket room; a
    /// message that can't be written yet stays queued (and keeps aging). Returns whether
    /// the message ended the session.
    async fn flush_one(&self, sock: &SeqSocket, shared: &Shared) -> io::Result<bool> {
        loop {
            if !self.has_pending() {
                self.wake.notified().await;
                continue;
            }
            let mut g = sock.fd.writable().await?;
            // The queue may have changed (or aged) while we waited.
            let Some(c) = self.candidate(shared) else {
                continue;
            };
            match g.try_io(|f| raw_send(f.as_raw_fd(), &c.bytes)) {
                Ok(Ok(())) => {
                    self.commit(&c, shared);
                    return Ok(c.terminal);
                }
                Ok(Err(e)) => return Err(e),
                Err(_would_block) => continue,
            }
        }
    }
}

async fn session_task(core: CoreRef, sock: SeqSocket) {
    let shared = core.borrow().shared.clone();
    let deny = |sock: &SeqSocket, reason: u8| {
        let _ = sock.try_send_msg(&RtMsg::Refused { reason });
    };
    let first = tokio::time::timeout(HELLO_TIMEOUT, sock.recv_raw()).await;
    let Ok(Ok(Some(bytes))) = first else { return };
    let (role, takeover, incarnation, flags) = match RtMsg::decode(&bytes) {
        Ok(RtMsg::Hello {
            proto,
            role,
            takeover,
            incarnation,
            flags,
        }) => {
            if proto != RT_PROTO {
                return deny(&sock, refuse::VERSION);
            }
            if role == Role::Voice
                && (incarnation == 0 || flags & !(voice_state::LISTENING | voice_state::MIC) != 0)
            {
                return deny(&sock, refuse::BAD_HELLO);
            }
            (role, takeover, incarnation, flags)
        }
        _ => return deny(&sock, refuse::BAD_HELLO),
    };
    let acquired = core
        .borrow_mut()
        .acquire(role, takeover, incarnation, flags);
    let sess = match acquired {
        Ok(s) => s,
        Err(reason) => return deny(&sock, reason),
    };
    let ttl = core.borrow().ttl;
    sess.last_rx.set(Instant::now());
    loop {
        let lease_deadline = sess.last_rx.get() + ttl;
        let closing = sess.close_by.get();
        tokio::select! {
            biased;
            r = sess.flush_one(&sock, &shared) => match r {
                Ok(false) => {}
                Ok(true) | Err(_) => break,
            },
            r = sock.recv_raw(), if closing.is_none() => match r {
                Ok(Some(b)) => {
                    match RtMsg::decode(&b) {
                        Ok(m) => {
                            sess.last_rx.set(Instant::now());
                            on_client_msg(&core, &sess, m);
                        }
                        Err(_) => bump(&shared.counters.decode),
                    }
                }
                Ok(None) | Err(_) => break,
            },
            _ = tokio::time::sleep_until(lease_deadline), if closing.is_none() => {
                core.borrow_mut().release(&sess);
                sess.revoke(revoke::EXPIRED);
            }
            _ = tokio::time::sleep_until(closing.unwrap_or(lease_deadline)), if closing.is_some() => break,
        }
    }
    core.borrow_mut().release(&sess);
}

fn on_client_msg(core: &CoreRef, sess: &Rc<Session>, m: RtMsg) {
    let mut c = core.borrow_mut();
    match m {
        RtMsg::VoiceOut { sent_mono_us, env } => c.on_voice_out(sess, sent_mono_us, env),
        RtMsg::PoseOut {
            source_mono_us,
            seq,
            pose,
        } => c.on_pose(sess, source_mono_us, seq, pose),
        RtMsg::SetState { flags, incarnation } => c.on_set_state(sess, flags, incarnation),
        RtMsg::Keepalive => {}
        // A second hello, or a daemon-to-client message: ignored (but it renewed the lease).
        _ => bump(&c.shared.counters.decode),
    }
}
