//! Runs one [`Room`] on tokio.
//!
//! The driver owns the room and is its only caller. It feeds [`Input`]s with a
//! [`Now`] taken from a monotonic clock and the wall clock, executes [`Output`]s, and
//! sleeps until `poll_timeout`. It is generic over [`Transport`], so the same code runs
//! on iroh and on the in-memory network.
//!
//! Per connection the driver runs: a receive task, one send task per lane (strict
//! priority comes from the transport), and a datagram task that enforces `max_age_ms`
//! in the driver's own queue and holds datagrams back while the transport's buffer
//! is full, so they age here instead of being evicted silently.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use room_core::api::{
    ClientId, ConnId, Input, Lane, LocalCommand, LocalEvent, Output, Reply, Update,
};
use room_core::frame::Reason;
use room_core::ids::{Key, Now};
use room_core::room::Room;
use tokio::sync::{mpsc, oneshot, Notify};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;

use crate::rt::{RtJson, RtShared, ViewEntry};
use crate::state;
use crate::transport::{ConnStats, Connection, Route, Transport};

#[derive(Clone, Debug)]
pub struct DriverConfig {
    /// Where `Persisted::encode` is written on `PersistDirty` (debounced). `None` = never.
    pub persist_path: Option<PathBuf>,
    pub persist_debounce: Duration,
    /// Datagrams waiting per connection before the oldest is dropped.
    pub datagram_queue: usize,
    /// Frame bytes waiting per connection (all lanes) before it is closed as overloaded.
    pub conn_queue_bytes: usize,
    /// Per-client event queue; a client that lets it fill is detached.
    pub client_queue: usize,
    /// The realtime voice lane's shared half. With it, datagram kinds 2 and 3 bypass the
    /// core (stamped in the receive task) and the driver publishes the membership view.
    pub rt: Option<RtShared>,
}

impl Default for DriverConfig {
    fn default() -> Self {
        Self {
            persist_path: None,
            persist_debounce: Duration::from_millis(500),
            datagram_queue: 128,
            conn_queue_bytes: 16 << 20,
            client_queue: 1024,
            rt: None,
        }
    }
}

/// What the driver hands a local API client.
#[derive(Debug, Clone)]
pub struct ClientMsg {
    pub event: LocalEvent,
    /// Filled for `Reply::Status`: one entry per live connection.
    pub routes: Option<Vec<RouteInfo>>,
    /// Filled for `Reply::Status` when the realtime lane runs.
    pub rt: Option<Box<RtJson>>,
}

#[derive(Debug, Clone)]
pub struct RouteInfo {
    pub peer: Key,
    pub conn: ConnId,
    pub outbound: bool,
    pub route: Route,
    pub stats: ConnStats,
    pub datagrams_sent: u64,
    /// Dropped in the driver queue because they were older than their `max_age_ms`.
    pub datagrams_dropped_age: u64,
    /// Dropped in the driver queue because it was full.
    pub datagrams_dropped_queue: u64,
}

enum Ctl {
    Attach {
        client: ClientId,
        tx: mpsc::Sender<ClientMsg>,
    },
    Detach {
        client: ClientId,
    },
    Local {
        client: ClientId,
        cmd: LocalCommand,
    },
    Drain {
        client: ClientId,
        sub: u32,
        max: usize,
        reply: oneshot::Sender<Vec<Update>>,
    },
    Shutdown {
        done: oneshot::Sender<()>,
    },
}

#[derive(Clone)]
pub struct DriverHandle {
    ctl: mpsc::UnboundedSender<Ctl>,
    next_client: Arc<AtomicU64>,
    queue: usize,
}

impl DriverHandle {
    /// A new local API session. The core sees `Attach` before anything else.
    pub fn attach(&self) -> (ClientId, mpsc::Receiver<ClientMsg>) {
        let client = self.next_client.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(self.queue);
        let _ = self.ctl.send(Ctl::Attach { client, tx });
        (client, rx)
    }

    pub fn detach(&self, client: ClientId) {
        let _ = self.ctl.send(Ctl::Detach { client });
    }

    pub fn local(&self, client: ClientId, cmd: LocalCommand) {
        let _ = self.ctl.send(Ctl::Local { client, cmd });
    }

    /// Coalesced updates of one subscription, at most `max`.
    pub async fn drain(&self, client: ClientId, sub: u32, max: usize) -> Vec<Update> {
        let (reply, rx) = oneshot::channel();
        if self
            .ctl
            .send(Ctl::Drain {
                client,
                sub,
                max,
                reply,
            })
            .is_err()
        {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    /// Save state, close every connection and stop the driver.
    pub async fn shutdown(&self) {
        let (done, rx) = oneshot::channel();
        if self.ctl.send(Ctl::Shutdown { done }).is_ok() {
            let _ = rx.await;
        }
    }
}

enum Msg<C> {
    Accepted(C),
    Dialed {
        peer: Key,
        result: Result<C, String>,
    },
    Frame {
        id: ConnId,
        lane: Lane,
        bytes: Vec<u8>,
    },
    Datagram {
        id: ConnId,
        bytes: Vec<u8>,
    },
    Closed {
        id: ConnId,
    },
}

struct DgramItem {
    at: Instant,
    bytes: Vec<u8>,
    max_age_ms: u32,
    /// 0 for ordinary datagrams; a voice stream's incarnation for voice (session fencing).
    tag: u32,
}

#[derive(Default)]
pub(crate) struct DgramQueue {
    q: Mutex<VecDeque<DgramItem>>,
    notify: Notify,
    closed: AtomicBool,
    sent: AtomicU64,
    dropped_age: AtomicU64,
    dropped_queue: AtomicU64,
}

impl DgramQueue {
    /// Queue a datagram, dropping the oldest while `cap` is reached.
    pub(crate) fn push(&self, bytes: Vec<u8>, max_age_ms: u32, tag: u32, cap: usize) {
        {
            let mut q = self.q.lock().unwrap();
            while q.len() >= cap.max(1) {
                q.pop_front();
                self.dropped_queue.fetch_add(1, Ordering::Relaxed);
            }
            q.push_back(DgramItem {
                at: Instant::now(),
                bytes,
                max_age_ms,
                tag,
            });
        }
        self.notify.notify_one();
    }

    /// Drop queued voice datagrams of any stream but `keep` (session fencing).
    pub(crate) fn purge_tagged_except(&self, keep: u32) {
        self.q
            .lock()
            .unwrap()
            .retain(|i| i.tag == 0 || i.tag == keep);
    }
}

struct ConnEntry<C> {
    conn: C,
    peer: Key,
    outbound: bool,
    lane_tx: [Option<mpsc::UnboundedSender<Vec<u8>>>; 3],
    lane_tasks: Vec<JoinHandle<()>>,
    queued_bytes: Arc<AtomicUsize>,
    dgrams: Arc<DgramQueue>,
    closing: bool,
    tasks: Vec<JoinHandle<()>>,
}

pub struct Driver<T: Transport> {
    room: Room,
    transport: Arc<T>,
    cfg: DriverConfig,
    start: Instant,
    ctl_rx: mpsc::UnboundedReceiver<Ctl>,
    rx: mpsc::Receiver<Msg<T::Conn>>,
    tx: mpsc::Sender<Msg<T::Conn>>,
    conns: HashMap<ConnId, ConnEntry<T::Conn>>,
    next_conn: ConnId,
    dialing: HashSet<Key>,
    clients: HashMap<ClientId, mpsc::Sender<ClientMsg>>,
    dirty_since: Option<Instant>,
    background: JoinSet<()>,
}

impl<T: Transport> Driver<T> {
    /// Start driving `room` on the current runtime.
    pub fn spawn(room: Room, transport: T, cfg: DriverConfig) -> (DriverHandle, JoinHandle<()>)
    where
        Room: Send,
    {
        let (ctl, ctl_rx) = mpsc::unbounded_channel();
        let (tx, rx) = mpsc::channel(1024);
        let handle = DriverHandle {
            ctl,
            next_client: Arc::new(AtomicU64::new(1)),
            queue: cfg.client_queue,
        };
        let mut driver = Driver {
            room,
            transport: Arc::new(transport),
            cfg,
            start: Instant::now(),
            ctl_rx,
            rx,
            tx,
            conns: HashMap::new(),
            next_conn: 1,
            dialing: HashSet::new(),
            clients: HashMap::new(),
            dirty_since: None,
            background: JoinSet::new(),
        };
        let task = tokio::spawn(async move { driver.run().await });
        (handle, task)
    }

    fn now(&self) -> Now {
        Now {
            mono_us: self.start.elapsed().as_micros() as u64,
            unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        }
    }

    fn feed(&mut self, input: Input) {
        tracing::trace!(?input, "input");
        let now = self.now();
        self.room.handle(now, input);
        self.pump();
    }

    async fn run(&mut self) {
        // Accept loop.
        {
            let transport = self.transport.clone();
            let tx = self.tx.clone();
            self.background.spawn(async move {
                while let Some(c) = transport.accept().await {
                    if tx.send(Msg::Accepted(c)).await.is_err() {
                        break;
                    }
                }
            });
        }
        // A freshly built room may owe work that no deadline announces (a joiner's first
        // dial is due "now"), so give it a tick before the first sleep.
        self.feed(Input::Tick);
        let mut shutdown: Option<oneshot::Sender<()>> = None;
        loop {
            let deadline = self.next_deadline();
            tokio::select! {
                biased;
                ctl = self.ctl_rx.recv() => match ctl {
                    Some(Ctl::Shutdown { done }) => { shutdown = Some(done); break; }
                    Some(c) => self.on_ctl(c),
                    None => break,
                },
                msg = self.rx.recv() => {
                    if let Some(m) = msg { self.on_msg(m); }
                }
                _ = sleep_until_opt(deadline) => self.on_timer(),
            }
        }
        self.finish().await;
        if let Some(done) = shutdown {
            let _ = done.send(());
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        let mut d = self
            .room
            .poll_timeout()
            .map(|us| self.start + Duration::from_micros(us));
        if let Some(since) = self.dirty_since {
            let p = since + self.cfg.persist_debounce;
            d = Some(d.map_or(p, |x| x.min(p)));
        }
        d
    }

    fn on_timer(&mut self) {
        let now = self.now();
        if self.room.poll_timeout().is_some_and(|t| t <= now.mono_us) {
            self.feed(Input::Tick);
        }
        if let Some(since) = self.dirty_since {
            if since.elapsed() >= self.cfg.persist_debounce {
                self.persist_now();
            }
        }
    }

    fn on_ctl(&mut self, ctl: Ctl) {
        match ctl {
            Ctl::Attach { client, tx } => {
                self.clients.insert(client, tx);
                self.feed(Input::Local {
                    client,
                    cmd: LocalCommand::Attach,
                });
            }
            Ctl::Detach { client } => {
                if self.clients.remove(&client).is_some() {
                    self.feed(Input::Local {
                        client,
                        cmd: LocalCommand::Detach,
                    });
                }
            }
            Ctl::Local { client, cmd } => self.feed(Input::Local { client, cmd }),
            Ctl::Drain {
                client,
                sub,
                max,
                reply,
            } => {
                let updates = self.room.drain(client, sub, max);
                let _ = reply.send(updates);
            }
            Ctl::Shutdown { .. } => unreachable!("handled in run"),
        }
    }

    fn on_msg(&mut self, msg: Msg<T::Conn>) {
        match msg {
            Msg::Accepted(conn) => self.add_conn(conn),
            Msg::Dialed { peer, result } => {
                self.dialing.remove(&peer);
                match result {
                    Ok(conn) => self.add_conn(conn),
                    Err(e) => {
                        tracing::debug!("dial {} failed: {e}", peer.short());
                        self.feed(Input::DialFailed { peer });
                    }
                }
            }
            Msg::Frame { id, lane, bytes } => {
                if self.conns.get(&id).is_some_and(|e| !e.closing) {
                    self.feed(Input::Frame {
                        conn: id,
                        lane,
                        bytes,
                    });
                }
            }
            Msg::Datagram { id, bytes } => {
                if self.conns.get(&id).is_some_and(|e| !e.closing) {
                    self.feed(Input::Datagram { conn: id, bytes });
                }
            }
            Msg::Closed { id } => {
                if let Some(e) = self.conns.remove(&id) {
                    if let Some(rt) = &self.cfg.rt {
                        rt.unestablished(id);
                    }
                    e.dgrams.closed.store(true, Ordering::Relaxed);
                    e.dgrams.notify.notify_one();
                    for t in e.tasks.iter().chain(e.lane_tasks.iter()) {
                        t.abort();
                    }
                    self.feed(Input::Disconnected { conn: id });
                }
            }
        }
    }

    fn add_conn(&mut self, conn: T::Conn) {
        let id = self.next_conn;
        self.next_conn += 1;
        let peer = conn.peer();
        let outbound = conn.outbound();
        let queued_bytes = Arc::new(AtomicUsize::new(0));
        let dgrams = Arc::new(DgramQueue::default());
        let mut lane_tx: [Option<mpsc::UnboundedSender<Vec<u8>>>; 3] = Default::default();
        let mut lane_tasks = Vec::new();
        for (i, slot) in lane_tx.iter_mut().enumerate() {
            let (tx, rx) = mpsc::unbounded_channel();
            *slot = Some(tx);
            let lane = match i {
                0 => Lane::Control,
                1 => Lane::State,
                _ => Lane::Bulk,
            };
            lane_tasks.push(tokio::spawn(lane_task(
                conn.clone(),
                lane,
                rx,
                queued_bytes.clone(),
            )));
        }
        let tasks = vec![
            tokio::spawn(recv_task(conn.clone(), id, self.tx.clone())),
            tokio::spawn(recv_dgram_task(
                conn.clone(),
                id,
                self.tx.clone(),
                self.cfg.rt.clone(),
            )),
            tokio::spawn(dgram_task(conn.clone(), dgrams.clone())),
        ];
        self.conns.insert(
            id,
            ConnEntry {
                conn,
                peer,
                outbound,
                lane_tx,
                lane_tasks,
                queued_bytes,
                dgrams,
                closing: false,
                tasks,
            },
        );
        self.feed(Input::Connected {
            conn: id,
            peer,
            outbound,
        });
    }

    /// Execute every output the room has ready.
    fn pump(&mut self) {
        while let Some(out) = self.room.poll_output() {
            self.exec(out);
        }
    }

    fn exec(&mut self, out: Output) {
        tracing::trace!(?out, "output");
        match out {
            Output::Dial { peer, hints } => {
                if !self.dialing.insert(peer) {
                    return;
                }
                let transport = self.transport.clone();
                let tx = self.tx.clone();
                self.background.spawn(async move {
                    let result = transport.dial(peer, hints).await.map_err(|e| e.0);
                    let _ = tx.send(Msg::Dialed { peer, result }).await;
                });
            }
            Output::Send { conn, lane, bytes } => {
                let cap = self.cfg.conn_queue_bytes;
                let Some(e) = self.conns.get_mut(&conn) else {
                    return;
                };
                if e.closing {
                    return;
                }
                let queued = e.queued_bytes.load(Ordering::Relaxed);
                if queued + bytes.len() > cap {
                    tracing::warn!("conn {conn} send queue over {cap} bytes; closing");
                    Self::begin_close(e, Reason::Shutdown);
                    return;
                }
                e.queued_bytes.fetch_add(bytes.len(), Ordering::Relaxed);
                if let Some(tx) = &e.lane_tx[lane as usize] {
                    let _ = tx.send(bytes);
                }
            }
            Output::SendDatagram {
                conn,
                bytes,
                max_age_ms,
            } => {
                let Some(e) = self.conns.get(&conn) else {
                    return;
                };
                if e.closing {
                    return;
                }
                e.dgrams.push(bytes, max_age_ms, 0, self.cfg.datagram_queue);
            }
            Output::Close { conn, reason } => {
                if let Some(e) = self.conns.get_mut(&conn) {
                    Self::begin_close(e, reason);
                }
            }
            Output::Local { client, event } => {
                let is_status = matches!(
                    &event,
                    LocalEvent::Reply {
                        reply: Reply::Status(_),
                        ..
                    }
                );
                let routes = is_status.then(|| self.routes());
                let rt = if is_status {
                    self.cfg.rt.as_ref().map(|r| Box::new(r.snapshot()))
                } else {
                    None
                };
                let overflow = match self.clients.get(&client) {
                    Some(tx) => tx.try_send(ClientMsg { event, routes, rt }).is_err(),
                    None => false,
                };
                if overflow {
                    tracing::warn!("client {client} is not keeping up; detaching it");
                    self.clients.remove(&client);
                    let now = self.now();
                    self.room.handle(
                        now,
                        Input::Local {
                            client,
                            cmd: LocalCommand::Detach,
                        },
                    );
                    // Outputs of that detach are picked up by the enclosing pump loop.
                }
            }
            Output::Established { conn, peer, caps } => {
                if let (Some(rt), Some(e)) = (&self.cfg.rt, self.conns.get(&conn)) {
                    rt.established(
                        conn,
                        ViewEntry {
                            peer,
                            voice: caps & room_core::frame::caps::VOICE != 0,
                            dgrams: e.dgrams.clone(),
                            cap: self.cfg.datagram_queue,
                        },
                    );
                }
            }
            Output::Unestablished { conn } => {
                if let Some(rt) = &self.cfg.rt {
                    rt.unestablished(conn);
                }
            }
            Output::PersistDirty => {
                if self.dirty_since.is_none() {
                    self.dirty_since = Some(Instant::now());
                }
            }
        }
    }

    /// Flush what is queued, then close the transport connection.
    fn begin_close(e: &mut ConnEntry<T::Conn>, reason: Reason) {
        if e.closing {
            return;
        }
        e.closing = true;
        for s in e.lane_tx.iter_mut() {
            *s = None; // lane tasks drain their queues, then end
        }
        let lanes = std::mem::take(&mut e.lane_tasks);
        let conn = e.conn.clone();
        e.tasks.push(tokio::spawn(async move {
            for t in lanes {
                let _ = t.await;
            }
            conn.close(reason);
        }));
    }

    pub fn routes(&self) -> Vec<RouteInfo> {
        let mut v: Vec<RouteInfo> = self
            .conns
            .iter()
            .filter(|(_, e)| !e.closing)
            .map(|(id, e)| RouteInfo {
                peer: e.peer,
                conn: *id,
                outbound: e.outbound,
                route: e.conn.route(),
                stats: e.conn.stats(),
                datagrams_sent: e.dgrams.sent.load(Ordering::Relaxed),
                datagrams_dropped_age: e.dgrams.dropped_age.load(Ordering::Relaxed),
                datagrams_dropped_queue: e.dgrams.dropped_queue.load(Ordering::Relaxed),
            })
            .collect();
        v.sort_by_key(|r| (r.peer, r.conn));
        v
    }

    fn persist_now(&mut self) {
        self.dirty_since = None;
        if let Some(path) = &self.cfg.persist_path {
            let bytes = self.room.persist().encode();
            if let Err(e) = state::atomic_write(path, &bytes, 0o600) {
                tracing::error!("persisting {}: {e}", path.display());
            }
        }
    }

    async fn finish(&mut self) {
        if self.dirty_since.is_some() {
            self.persist_now();
        }
        for e in self.conns.values_mut() {
            Self::begin_close(e, Reason::Shutdown);
        }
        // Give closers a moment to flush, then drop the network.
        let tasks: Vec<JoinHandle<()>> = self
            .conns
            .values_mut()
            .flat_map(|e| std::mem::take(&mut e.tasks))
            .collect();
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            for t in tasks {
                let _ = t.await;
            }
        })
        .await;
        self.background.abort_all();
        self.transport.shutdown().await;
    }
}

async fn sleep_until_opt(d: Option<Instant>) {
    match d {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending().await,
    }
}

async fn lane_task<C: Connection>(
    conn: C,
    lane: Lane,
    mut rx: mpsc::UnboundedReceiver<Vec<u8>>,
    queued: Arc<AtomicUsize>,
) {
    while let Some(bytes) = rx.recv().await {
        let n = bytes.len();
        let r = conn.send_frame(lane, bytes).await;
        queued.fetch_sub(n, Ordering::Relaxed);
        if r.is_err() {
            break;
        }
    }
}

/// Frames for the core. Datagrams have their own task: they must never wait behind a
/// frame that is blocked on a busy core.
async fn recv_task<C: Connection>(conn: C, id: ConnId, tx: mpsc::Sender<Msg<C>>) {
    while let Some((lane, bytes)) = conn.recv_frame().await {
        if tx.send(Msg::Frame { id, lane, bytes }).await.is_err() {
            return;
        }
    }
    let _ = tx.send(Msg::Closed { id }).await;
}

/// Datagrams: voice and visemes go to the realtime lane, stamped right here; the rest go
/// to the core without waiting for it.
async fn recv_dgram_task<C: Connection>(
    conn: C,
    id: ConnId,
    tx: mpsc::Sender<Msg<C>>,
    rt: Option<RtShared>,
) {
    while let Some(bytes) = conn.recv_datagram().await {
        let bytes = match &rt {
            Some(rt) => match rt.offer(id, bytes) {
                Some(b) => b,
                None => continue,
            },
            None => bytes,
        };
        let _ = tx.try_send(Msg::Datagram { id, bytes });
    }
}

async fn dgram_task<C: Connection>(conn: C, q: Arc<DgramQueue>) {
    enum Step {
        Idle,
        Wait,
        Send(Vec<u8>),
    }
    loop {
        let notified = q.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        loop {
            // Read the space before taking the queue lock: `datagram_space()` goes into
            // the QUIC connection, and the rt thread's `push` waits on `q.q`.
            let space = conn.datagram_space();
            let step = {
                let mut g = q.q.lock().unwrap();
                match g.front() {
                    None => Step::Idle,
                    Some(item) => {
                        if item.max_age_ms > 0
                            && item.at.elapsed() > Duration::from_millis(item.max_age_ms as u64)
                        {
                            g.pop_front();
                            q.dropped_age.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                        if space < item.bytes.len() {
                            Step::Wait
                        } else {
                            Step::Send(g.pop_front().unwrap().bytes)
                        }
                    }
                }
            };
            match step {
                Step::Idle => break,
                Step::Wait => tokio::time::sleep(Duration::from_millis(2)).await,
                Step::Send(bytes) => {
                    conn.send_datagram(bytes);
                    q.sent.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        if q.closed.load(Ordering::Relaxed) {
            return;
        }
        notified.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purge_keeps_other_traffic_and_the_current_voice_stream() {
        let q = DgramQueue::default();
        q.push(vec![1], 40, 5, 128); // old voice stream
        q.push(vec![2], 0, 0, 128); // not voice
        q.push(vec![3], 40, 6, 128); // current voice stream
        q.push(vec![4], 40, 5, 128);
        q.purge_tagged_except(6);
        let kept: Vec<u8> = q.q.lock().unwrap().iter().map(|i| i.bytes[0]).collect();
        assert_eq!(kept, vec![2, 3]);
    }

    #[test]
    fn push_drops_the_oldest_at_capacity() {
        let q = DgramQueue::default();
        for i in 0..5u8 {
            q.push(vec![i], 0, 0, 3);
        }
        let kept: Vec<u8> = q.q.lock().unwrap().iter().map(|i| i.bytes[0]).collect();
        assert_eq!(kept, vec![2, 3, 4]);
        assert_eq!(q.dropped_queue.load(Ordering::Relaxed), 2);
    }
}
