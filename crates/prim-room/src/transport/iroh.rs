//! QUIC transport over iroh 1.4 (ALPN `prim/1`).
//!
//! * One QUIC connection per peer. The core resolves duplicates; the transport lets two
//!   transient connections coexist.
//! * Lane 0 is the bidirectional stream the dialler opens. Lanes 1 and 2 are
//!   unidirectional streams each side opens lazily, with the lane id as first byte.
//! * Priorities (`SendStream::set_priority`): lane 0 = 2, lane 1 = 1, lane 2 = 0.
//! * One writer task per lane drains a bounded queue, so `send_frame` (a
//!   `reserve` + `send` on that queue) is cancel-safe and frames reach a stream whole.
//! * Datagrams ride the same connection; `max_datagram_size()` is checked before
//!   every send.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use iroh::endpoint::{presets, Connection as QConn, ConnectionError, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, RelayUrl, SecretKey};
use room_core::api::Lane;
use room_core::frame::{Reason, ALPN};
use room_core::ids::Key;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use super::pipe::Pipe;
use super::{
    CloseInfo, ConnClosed, ConnStats, Connection, DatagramSend, RawFrameReader, Received, Route,
    RouteKind, Transport, TransportError,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelaySetting {
    /// n0's public relays and address lookup (DNS).
    N0,
    /// No relay and no address lookup: only direct addresses work.
    Disabled,
    /// These relay URLs, no address lookup.
    Custom(Vec<String>),
}

/// Direct addresses by member key, read at every dial. Shared so a caller can add
/// addresses after the endpoint is bound (a test fleet registers each node as it comes up).
pub type PeerAddrs = Arc<Mutex<HashMap<Key, Vec<SocketAddr>>>>;

#[derive(Clone, Debug)]
pub struct IrohConfig {
    pub relay: RelaySetting,
    /// Bind one socket here (v4 or v6) instead of the defaults.
    pub bind: Option<SocketAddr>,
    /// Extra direct addresses to try for specific peers.
    pub peer_addrs: PeerAddrs,
    /// Frames queued per lane before `send_frame` waits.
    pub lane_cap: usize,
    /// Datagram receive queue size in bytes.
    pub datagram_queue_bytes: usize,
    pub dial_timeout: Duration,
    /// Use this nameserver instead of the system's (a netns whose `resolv.conf` points at
    /// a host-local stub resolver needs it).
    pub dns_server: Option<SocketAddr>,
    /// Wait this long for the endpoint to know its home relay before returning from `bind`.
    pub online_timeout: Duration,
}

impl Default for IrohConfig {
    fn default() -> Self {
        Self {
            relay: RelaySetting::N0,
            bind: None,
            peer_addrs: PeerAddrs::default(),
            lane_cap: 32,
            datagram_queue_bytes: 64 * 1024,
            dial_timeout: Duration::from_secs(20),
            dns_server: None,
            online_timeout: Duration::from_secs(8),
        }
    }
}

pub struct IrohTransport {
    endpoint: Endpoint,
    key: Key,
    cfg: IrohConfig,
    accept_rx: tokio::sync::Mutex<mpsc::Receiver<IrohConn>>,
    accept_task: JoinHandle<()>,
}

fn qkey(k: &Key) -> Result<EndpointId, TransportError> {
    EndpointId::from_bytes(&k.0).map_err(|e| TransportError(format!("bad key: {e}")))
}

impl IrohTransport {
    /// Bind an endpoint whose secret key is `secret` (so its id is the member key).
    pub async fn bind(secret: [u8; 32], cfg: IrohConfig) -> Result<Self, TransportError> {
        let mut b = match &cfg.relay {
            RelaySetting::N0 => Endpoint::builder(presets::N0),
            RelaySetting::Disabled => {
                Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled)
            }
            RelaySetting::Custom(urls) => {
                let mut parsed = Vec::new();
                for u in urls {
                    parsed.push(
                        u.parse::<RelayUrl>()
                            .map_err(|e| TransportError(format!("relay url {u}: {e}")))?,
                    );
                }
                Endpoint::builder(presets::Minimal).relay_mode(RelayMode::custom(parsed))
            }
        };
        b = b
            .secret_key(SecretKey::from_bytes(&secret))
            .alpns(vec![ALPN.to_vec()]);
        if let Some(ns) = cfg.dns_server {
            b = b.dns_resolver(iroh::dns::DnsResolver::with_nameserver(ns));
        }
        if let Some(addr) = cfg.bind {
            b = b
                .clear_ip_transports()
                .bind_addr(addr)
                .map_err(|e| TransportError(format!("bind address: {e}")))?;
        }
        let endpoint = b
            .bind()
            .await
            .map_err(|e| TransportError(format!("bind: {e}")))?;
        if cfg.relay != RelaySetting::Disabled {
            let _ = tokio::time::timeout(cfg.online_timeout, endpoint.online()).await;
        }
        let key = Key(*endpoint.id().as_bytes());
        let (tx, rx) = mpsc::channel(16);
        let accept_task = {
            let endpoint = endpoint.clone();
            let cfg = cfg.clone();
            tokio::spawn(async move {
                while let Some(incoming) = endpoint.accept().await {
                    let tx = tx.clone();
                    let cfg = cfg.clone();
                    tokio::spawn(async move {
                        match incoming.await {
                            Ok(conn) => {
                                let _ = tx.send(IrohConn::new(conn, false, &cfg)).await;
                            }
                            Err(e) => tracing::debug!("inbound handshake failed: {e}"),
                        }
                    });
                }
            })
        };
        Ok(Self {
            endpoint,
            key,
            cfg,
            accept_rx: tokio::sync::Mutex::new(rx),
            accept_task,
        })
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Sockets the endpoint is bound to (for building direct-address hints in tests).
    pub fn bound_sockets(&self) -> Vec<SocketAddr> {
        self.endpoint.bound_sockets()
    }

    fn addr_for(&self, peer: &Key, hints: &[String]) -> Result<EndpointAddr, TransportError> {
        let mut addr = EndpointAddr::new(qkey(peer)?);
        if let Some(list) = self.cfg.peer_addrs.lock().unwrap().get(peer) {
            for a in list {
                addr = addr.with_ip_addr(*a);
            }
        }
        for h in hints {
            if let Ok(sa) = h.parse::<SocketAddr>() {
                addr = addr.with_ip_addr(sa);
            } else if let Ok(url) = h.parse::<RelayUrl>() {
                addr = addr.with_relay_url(url);
            }
        }
        Ok(addr)
    }
}

impl Drop for IrohTransport {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

impl Transport for IrohTransport {
    type Conn = IrohConn;

    fn local_key(&self) -> Key {
        self.key
    }

    async fn dial(&self, peer: Key, hints: Vec<String>) -> Result<IrohConn, TransportError> {
        let addr = self.addr_for(&peer, &hints)?;
        let conn = tokio::time::timeout(self.cfg.dial_timeout, self.endpoint.connect(addr, ALPN))
            .await
            .map_err(|_| TransportError("dial timed out".into()))?
            .map_err(|e| TransportError(format!("dial: {e}")))?;
        Ok(IrohConn::new(conn, true, &self.cfg))
    }

    async fn accept(&self) -> Option<IrohConn> {
        self.accept_rx.lock().await.recv().await
    }

    async fn shutdown(&self) {
        self.accept_task.abort();
        self.endpoint.close().await;
    }
}

const PRIORITY: [i32; 3] = [2, 1, 0];
const FLUSH_WAIT: Duration = Duration::from_secs(2);

struct Inner {
    conn: QConn,
    peer: Key,
    outbound: bool,
    lane_tx: Mutex<[Option<mpsc::Sender<Vec<u8>>>; 3]>,
    writers: Mutex<Vec<JoinHandle<()>>>,
    inbox: Pipe,
    closed_tx: watch::Sender<Option<CloseInfo>>,
    closing: AtomicBool,
    too_large: AtomicU64,
    dropped_out: AtomicU64,
}

#[derive(Clone)]
pub struct IrohConn {
    inner: Arc<Inner>,
}

impl IrohConn {
    fn new(conn: QConn, outbound: bool, cfg: &IrohConfig) -> Self {
        let peer = Key(*conn.remote_id().as_bytes());
        let (closed_tx, _) = watch::channel(None);
        let mut txs: [Option<mpsc::Sender<Vec<u8>>>; 3] = Default::default();
        let mut rxs = Vec::new();
        for slot in txs.iter_mut() {
            let (tx, rx) = mpsc::channel(cfg.lane_cap.max(1));
            *slot = Some(tx);
            rxs.push(rx);
        }
        let inner = Arc::new(Inner {
            conn: conn.clone(),
            peer,
            outbound,
            lane_tx: Mutex::new(txs),
            writers: Mutex::new(Vec::new()),
            inbox: Pipe::new(cfg.lane_cap.max(1), cfg.datagram_queue_bytes),
            closed_tx,
            closing: AtomicBool::new(false),
            too_large: AtomicU64::new(0),
            dropped_out: AtomicU64::new(0),
        });

        // Lane 0's two halves are created by the writer (dialler: open_bi) or by the
        // accept loop (acceptor: accept_bi) and handed across.
        let (l0_send_tx, l0_send_rx) = oneshot::channel::<SendStream>();
        let (l0_recv_tx, l0_recv_rx) = oneshot::channel::<RecvStream>();

        let mut writers = Vec::new();
        let mut rx_iter = rxs.into_iter();
        // Lane 0 writer.
        {
            let rx = rx_iter.next().unwrap();
            let c = conn.clone();
            writers.push(tokio::spawn(async move {
                let Some(first) = recv_first(rx).await else {
                    return;
                };
                let (mut rx, first) = first;
                let send = if outbound {
                    match c.open_bi().await {
                        Ok((send, recv)) => {
                            let _ = l0_recv_tx.send(recv);
                            send
                        }
                        Err(_) => return,
                    }
                } else {
                    match l0_send_rx.await {
                        Ok(s) => s,
                        Err(_) => return,
                    }
                };
                write_lane(send, PRIORITY[0], first, &mut rx).await;
            }));
        }
        // Lanes 1 and 2 writers.
        for lane in 1..3u8 {
            let rx = rx_iter.next().unwrap();
            let c = conn.clone();
            writers.push(tokio::spawn(async move {
                let Some((mut rx, first)) = recv_first(rx).await else {
                    return;
                };
                let Ok(mut send) = c.open_uni().await else {
                    return;
                };
                if send.write_all(&[lane]).await.is_err() {
                    return;
                }
                write_lane(send, PRIORITY[lane as usize], first, &mut rx).await;
            }));
        }
        *inner.writers.lock().unwrap() = writers;

        // Lane 0 reader.
        {
            let inner = inner.clone();
            let c = conn.clone();
            tokio::spawn(async move {
                let recv = if outbound {
                    match l0_recv_rx.await {
                        Ok(r) => r,
                        Err(_) => return,
                    }
                } else {
                    match c.accept_bi().await {
                        Ok((send, recv)) => {
                            let _ = l0_send_tx.send(send);
                            recv
                        }
                        Err(_) => return,
                    }
                };
                read_lane(inner, Lane::Control, recv).await;
            });
        }
        // Uni stream accept loop.
        {
            let inner = inner.clone();
            let c = conn.clone();
            tokio::spawn(async move {
                while let Ok(mut recv) = c.accept_uni().await {
                    let inner = inner.clone();
                    tokio::spawn(async move {
                        let mut id = [0u8; 1];
                        if recv.read_exact(&mut id).await.is_err() {
                            return;
                        }
                        let lane = match id[0] {
                            1 => Lane::State,
                            2 => Lane::Bulk,
                            _ => {
                                inner.conn.close(0x7ffu32.into(), b"bad lane");
                                return;
                            }
                        };
                        read_lane(inner, lane, recv).await;
                    });
                }
            });
        }
        // Datagram reader.
        {
            let inner = inner.clone();
            let c = conn.clone();
            tokio::spawn(async move {
                while let Ok(d) = c.read_datagram().await {
                    inner.inbox.push_datagram(d.to_vec());
                }
            });
        }
        // Close monitor.
        {
            let inner = inner.clone();
            let c = conn.clone();
            tokio::spawn(async move {
                let err = c.closed().await;
                let info = match err {
                    ConnectionError::ApplicationClosed(ac) => CloseInfo {
                        reason: u16::try_from(u64::from(ac.error_code))
                            .ok()
                            .map(Reason::from_u16),
                        local: false,
                    },
                    ConnectionError::LocallyClosed => CloseInfo {
                        reason: None,
                        local: true,
                    },
                    _ => CloseInfo {
                        reason: None,
                        local: false,
                    },
                };
                inner.closed_tx.send_if_modified(|v| {
                    if v.is_none() {
                        *v = Some(info);
                        true
                    } else {
                        false
                    }
                });
                inner.inbox.close();
            });
        }
        IrohConn { inner }
    }

    /// The underlying iroh connection (path details for diagnostics).
    pub fn quic(&self) -> &QConn {
        &self.inner.conn
    }
}

async fn recv_first(mut rx: mpsc::Receiver<Vec<u8>>) -> Option<(mpsc::Receiver<Vec<u8>>, Vec<u8>)> {
    let first = rx.recv().await?;
    Some((rx, first))
}

/// Write queued frames to one stream until the queue's senders are dropped, then
/// finish the stream and wait (bounded) for the peer to acknowledge everything.
async fn write_lane(
    mut send: SendStream,
    priority: i32,
    first: Vec<u8>,
    rx: &mut mpsc::Receiver<Vec<u8>>,
) {
    let _ = send.set_priority(priority);
    let mut next = Some(first);
    while let Some(frame) = next.take() {
        if send.write_all(&frame).await.is_err() {
            return;
        }
        next = rx.recv().await;
    }
    if send.finish().is_ok() {
        let _ = tokio::time::timeout(FLUSH_WAIT, send.stopped()).await;
    }
}

async fn read_lane(inner: Arc<Inner>, lane: Lane, mut recv: RecvStream) {
    let mut reader = RawFrameReader::default();
    loop {
        match recv.read_chunk(64 * 1024).await {
            Ok(Some(chunk)) => {
                reader.push(&chunk);
                loop {
                    match reader.next_frame() {
                        Ok(Some(frame)) => {
                            if inner.inbox.push_frame(lane, frame).await.is_err() {
                                return;
                            }
                        }
                        Ok(None) => break,
                        Err(_) => {
                            inner.conn.close(0x7feu32.into(), b"frame too large");
                            return;
                        }
                    }
                }
            }
            Ok(None) | Err(_) => return,
        }
    }
}

impl Connection for IrohConn {
    fn peer(&self) -> Key {
        self.inner.peer
    }

    fn outbound(&self) -> bool {
        self.inner.outbound
    }

    async fn send_frame(&self, lane: Lane, bytes: Vec<u8>) -> Result<(), ConnClosed> {
        let tx = self.inner.lane_tx.lock().unwrap()[lane as usize]
            .clone()
            .ok_or(ConnClosed)?;
        let permit = tx.reserve().await.map_err(|_| ConnClosed)?;
        permit.send(bytes);
        Ok(())
    }

    fn send_datagram(&self, bytes: Vec<u8>) -> DatagramSend {
        let c = &self.inner.conn;
        if self.inner.closing.load(Ordering::Relaxed) || c.close_reason().is_some() {
            return DatagramSend::Closed;
        }
        match c.max_datagram_size() {
            Some(max) if bytes.len() <= max => {}
            _ => {
                self.inner.too_large.fetch_add(1, Ordering::Relaxed);
                return DatagramSend::TooLarge;
            }
        }
        let evicts = c.datagram_send_buffer_space() < bytes.len();
        match c.send_datagram(Bytes::from(bytes)) {
            Ok(()) if evicts => {
                self.inner.dropped_out.fetch_add(1, Ordering::Relaxed);
                DatagramSend::QueuedDroppedOld
            }
            Ok(()) => DatagramSend::Queued,
            Err(iroh::endpoint::SendDatagramError::TooLarge) => {
                self.inner.too_large.fetch_add(1, Ordering::Relaxed);
                DatagramSend::TooLarge
            }
            Err(_) => DatagramSend::Closed,
        }
    }

    fn datagram_space(&self) -> usize {
        self.inner.conn.datagram_send_buffer_space()
    }

    async fn recv(&self) -> Option<Received> {
        self.inner.inbox.recv().await
    }

    fn close(&self, reason: Reason) {
        if self.inner.closing.swap(true, Ordering::SeqCst) {
            return;
        }
        // Dropping the senders lets each writer drain its queue, finish its stream and
        // wait for the peer's ack; only then do we close the connection.
        *self.inner.lane_tx.lock().unwrap() = Default::default();
        let writers: Vec<_> = self.inner.writers.lock().unwrap().drain(..).collect();
        let c = self.inner.conn.clone();
        tokio::spawn(async move {
            let _ = tokio::time::timeout(FLUSH_WAIT * 2, async {
                for w in writers {
                    let _ = w.await;
                }
            })
            .await;
            c.close((reason as u16 as u32).into(), b"");
        });
    }

    async fn closed(&self) -> CloseInfo {
        let mut rx = self.inner.closed_tx.subscribe();
        loop {
            if let Some(info) = *rx.borrow_and_update() {
                return info;
            }
            if rx.changed().await.is_err() {
                return CloseInfo {
                    reason: None,
                    local: false,
                };
            }
        }
    }

    fn route(&self) -> Route {
        let c = &self.inner.conn;
        let mut kind = RouteKind::Unknown;
        let mut rtt_ms = None;
        for p in c.paths().iter() {
            if p.is_selected() {
                kind = if p.is_relay() {
                    RouteKind::Relay
                } else {
                    RouteKind::Direct
                };
                rtt_ms = Some(p.rtt().as_millis() as u64);
            }
        }
        Route {
            kind,
            max_datagram_size: c.max_datagram_size(),
            rtt_ms,
        }
    }

    fn stats(&self) -> ConnStats {
        ConnStats {
            datagrams_dropped_out: self.inner.dropped_out.load(Ordering::Relaxed),
            datagrams_too_large: self.inner.too_large.load(Ordering::Relaxed),
            datagrams_dropped_in: self.inner.inbox.dropped(),
        }
    }
}
