//! In-memory transport: many nodes in one process, keyed by member key.
//!
//! It keeps the semantics of the real transport that tests depend on (see the module
//! docs of [`super`]): bounded lanes with backpressure, drop-oldest datagram queue,
//! whole-frame cancellation, receive-side lane priority, and close from either side
//! reaching both ends.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use room_core::api::Lane;
use room_core::frame::Reason;
use room_core::ids::Key;
use tokio::sync::{mpsc, watch};

use super::pipe::Pipe;
use super::{
    CloseInfo, ConnClosed, ConnStats, Connection, DatagramSend, Received, Route, RouteKind,
    Transport, TransportError,
};

#[derive(Clone, Debug)]
pub struct MemConfig {
    /// Frames queued per lane before `send_frame` waits.
    pub lane_cap: usize,
    /// Datagram queue size in bytes before the oldest are dropped.
    pub datagram_queue_bytes: usize,
    pub max_datagram_size: usize,
}

impl Default for MemConfig {
    fn default() -> Self {
        Self {
            lane_cap: 16,
            datagram_queue_bytes: 16 * 1024,
            max_datagram_size: 1200,
        }
    }
}

struct Node {
    accept_tx: mpsc::UnboundedSender<MemConn>,
    conns: Arc<Mutex<Vec<Weak<Core>>>>,
}

struct NetInner {
    nodes: HashMap<Key, Node>,
}

/// The shared "network" of one test or simulation.
#[derive(Clone)]
pub struct MemNetwork {
    inner: Arc<Mutex<NetInner>>,
    cfg: MemConfig,
}

impl Default for MemNetwork {
    fn default() -> Self {
        Self::new()
    }
}

impl MemNetwork {
    pub fn new() -> Self {
        Self::with_config(MemConfig::default())
    }

    pub fn with_config(cfg: MemConfig) -> Self {
        Self {
            inner: Arc::new(Mutex::new(NetInner {
                nodes: HashMap::new(),
            })),
            cfg,
        }
    }

    /// Attach a node. Panics if `key` is already attached.
    pub fn transport(&self, key: Key) -> MemTransport {
        let (accept_tx, accept_rx) = mpsc::unbounded_channel();
        let conns = Arc::new(Mutex::new(Vec::new()));
        let prev = self.inner.lock().unwrap().nodes.insert(
            key,
            Node {
                accept_tx,
                conns: conns.clone(),
            },
        );
        assert!(prev.is_none(), "key already attached to the network");
        let (shut_tx, shut_rx) = watch::channel(false);
        MemTransport {
            key,
            net: self.clone(),
            accept_rx: tokio::sync::Mutex::new(accept_rx),
            conns,
            shut_tx,
            shut_rx,
        }
    }
}

pub struct MemTransport {
    key: Key,
    net: MemNetwork,
    accept_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<MemConn>>,
    conns: Arc<Mutex<Vec<Weak<Core>>>>,
    shut_tx: watch::Sender<bool>,
    shut_rx: watch::Receiver<bool>,
}

impl MemTransport {
    fn close_all(&self) {
        // Only remove our own slot (it may have been re-attached after a shutdown).
        let mut inner = self.net.inner.lock().unwrap();
        if let Some(node) = inner.nodes.get(&self.key) {
            if Arc::ptr_eq(&node.conns, &self.conns) {
                inner.nodes.remove(&self.key);
            }
        }
        drop(inner);
        let list: Vec<_> = self.conns.lock().unwrap().drain(..).collect();
        for w in list {
            if let Some(core) = w.upgrade() {
                core.close(None, Reason::Shutdown);
            }
        }
        let _ = self.shut_tx.send(true);
    }
}

impl Drop for MemTransport {
    fn drop(&mut self) {
        self.close_all();
    }
}

impl Transport for MemTransport {
    type Conn = MemConn;

    fn local_key(&self) -> Key {
        self.key
    }

    async fn dial(&self, peer: Key, _hints: Vec<String>) -> Result<MemConn, TransportError> {
        if *self.shut_rx.borrow() {
            return Err(TransportError("transport shut down".into()));
        }
        let (target_tx, target_conns) = {
            let inner = self.net.inner.lock().unwrap();
            match inner.nodes.get(&peer) {
                Some(n) => (n.accept_tx.clone(), n.conns.clone()),
                None => return Err(TransportError(format!("no such peer {}", peer.short()))),
            }
        };
        let cfg = &self.net.cfg;
        let (closed_tx, _) = watch::channel(None);
        let core = Arc::new(Core {
            dialer_inbox: Pipe::new(cfg.lane_cap, cfg.datagram_queue_bytes),
            acceptor_inbox: Pipe::new(cfg.lane_cap, cfg.datagram_queue_bytes),
            closed_tx,
            max_datagram_size: cfg.max_datagram_size,
            too_large: AtomicU64::new(0),
        });
        let dialer_end = MemConn {
            core: core.clone(),
            dialer_side: true,
            peer,
        };
        let acceptor_end = MemConn {
            core: core.clone(),
            dialer_side: false,
            peer: self.key,
        };
        if target_tx.send(acceptor_end).is_err() {
            return Err(TransportError(format!("peer {} went away", peer.short())));
        }
        self.conns.lock().unwrap().push(Arc::downgrade(&core));
        target_conns.lock().unwrap().push(Arc::downgrade(&core));
        Ok(dialer_end)
    }

    async fn accept(&self) -> Option<MemConn> {
        let mut rx = self.accept_rx.lock().await;
        let mut shut = self.shut_rx.clone();
        loop {
            tokio::select! {
                c = rx.recv() => return c,
                r = shut.changed() => {
                    if r.is_err() || *shut.borrow() { return None; }
                }
            }
        }
    }

    async fn shutdown(&self) {
        self.close_all();
    }
}

struct Core {
    dialer_inbox: Pipe,
    acceptor_inbox: Pipe,
    /// `Some((closed by the dialer side?, reason))`.
    closed_tx: watch::Sender<Option<(Option<bool>, Reason)>>,
    max_datagram_size: usize,
    too_large: AtomicU64,
}

impl Core {
    fn close(&self, by_dialer: Option<bool>, reason: Reason) {
        self.closed_tx.send_if_modified(|v| {
            if v.is_none() {
                *v = Some((by_dialer, reason));
                true
            } else {
                false
            }
        });
        // Queued items stay deliverable: a close flushes.
        self.dialer_inbox.close();
        self.acceptor_inbox.close();
    }
}

#[derive(Clone)]
pub struct MemConn {
    core: Arc<Core>,
    dialer_side: bool,
    peer: Key,
}

impl MemConn {
    fn inbox(&self) -> &Pipe {
        if self.dialer_side {
            &self.core.dialer_inbox
        } else {
            &self.core.acceptor_inbox
        }
    }
    fn outbox(&self) -> &Pipe {
        if self.dialer_side {
            &self.core.acceptor_inbox
        } else {
            &self.core.dialer_inbox
        }
    }
}

impl Connection for MemConn {
    fn peer(&self) -> Key {
        self.peer
    }

    fn outbound(&self) -> bool {
        self.dialer_side
    }

    async fn send_frame(&self, lane: Lane, bytes: Vec<u8>) -> Result<(), ConnClosed> {
        self.outbox().push_frame(lane, bytes).await
    }

    fn send_datagram(&self, bytes: Vec<u8>) -> DatagramSend {
        if bytes.len() > self.core.max_datagram_size {
            self.core.too_large.fetch_add(1, Ordering::Relaxed);
            return DatagramSend::TooLarge;
        }
        self.outbox().push_datagram(bytes)
    }

    fn datagram_space(&self) -> usize {
        self.outbox().datagram_space()
    }

    async fn recv(&self) -> Option<Received> {
        self.inbox().recv().await
    }

    fn close(&self, reason: Reason) {
        self.core.close(Some(self.dialer_side), reason);
    }

    async fn closed(&self) -> CloseInfo {
        let mut rx = self.core.closed_tx.subscribe();
        loop {
            if let Some((by, reason)) = *rx.borrow_and_update() {
                return CloseInfo {
                    reason: Some(reason),
                    local: by == Some(self.dialer_side),
                };
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
        Route {
            kind: RouteKind::Mem,
            max_datagram_size: Some(self.core.max_datagram_size),
            rtt_ms: Some(0),
        }
    }

    fn stats(&self) -> ConnStats {
        ConnStats {
            datagrams_dropped_out: self.outbox().dropped(),
            datagrams_too_large: self.core.too_large.load(Ordering::Relaxed),
            datagrams_dropped_in: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn k(n: u8) -> Key {
        Key([n; 32])
    }

    async fn pair(net: &MemNetwork) -> (MemTransport, MemTransport, MemConn, MemConn) {
        let a = net.transport(k(1));
        let b = net.transport(k(2));
        let ca = a.dial(k(2), vec![]).await.unwrap();
        let cb = b.accept().await.unwrap();
        (a, b, ca, cb)
    }

    fn frame(tag: u8) -> Vec<u8> {
        let mut f = 1u32.to_le_bytes().to_vec();
        f.push(tag);
        f
    }

    #[tokio::test]
    async fn lifecycle_close_reaches_both_sides() {
        let net = MemNetwork::new();
        let (_a, _b, ca, cb) = pair(&net).await;
        assert_eq!(ca.peer(), k(2));
        assert_eq!(cb.peer(), k(1));
        assert!(ca.outbound() && !cb.outbound());
        ca.send_frame(Lane::Control, frame(1)).await.unwrap();
        ca.close(Reason::Duplicate);
        // Queued frames are flushed to the peer before it sees the close.
        assert_eq!(
            cb.recv().await,
            Some(Received::Frame {
                lane: Lane::Control,
                bytes: frame(1)
            })
        );
        assert_eq!(cb.recv().await, None);
        assert_eq!(ca.recv().await, None);
        let ia = ca.closed().await;
        let ib = cb.closed().await;
        assert_eq!((ia.reason, ia.local), (Some(Reason::Duplicate), true));
        assert_eq!((ib.reason, ib.local), (Some(Reason::Duplicate), false));
        assert_eq!(
            cb.send_frame(Lane::Control, frame(2)).await,
            Err(ConnClosed)
        );
        assert_eq!(cb.send_datagram(vec![1]), DatagramSend::Closed);
    }

    #[tokio::test]
    async fn lifecycle_close_from_acceptor_side() {
        let net = MemNetwork::new();
        let (_a, _b, ca, cb) = pair(&net).await;
        cb.close(Reason::Revoked);
        assert_eq!(ca.recv().await, None);
        let i = ca.closed().await;
        assert_eq!((i.reason, i.local), (Some(Reason::Revoked), false));
    }

    #[tokio::test]
    async fn lifecycle_dropping_transport_closes_connections() {
        let net = MemNetwork::new();
        let (a, b, ca, cb) = pair(&net).await;
        drop(a);
        assert_eq!(cb.recv().await, None);
        assert_eq!(ca.recv().await, None);
        assert_eq!(cb.closed().await.reason, Some(Reason::Shutdown));
        // The dropped node is gone: dialling it fails, but b is still attached.
        assert!(b.dial(k(1), vec![]).await.is_err());
    }

    #[tokio::test]
    async fn lifecycle_dial_absent_key_fails() {
        let net = MemNetwork::new();
        let a = net.transport(k(1));
        assert!(a.dial(k(9), vec![]).await.is_err());
        a.shutdown().await;
        assert!(a.accept().await.is_none());
        assert!(a.dial(k(1), vec![]).await.is_err());
    }

    #[tokio::test]
    async fn overload_lane_backpressure_and_datagram_drops() {
        let net = MemNetwork::with_config(MemConfig {
            lane_cap: 2,
            datagram_queue_bytes: 30,
            max_datagram_size: 20,
        });
        let (_a, _b, ca, cb) = pair(&net).await;
        ca.send_frame(Lane::State, frame(1)).await.unwrap();
        ca.send_frame(Lane::State, frame(2)).await.unwrap();
        // Third send must wait: the lane is full.
        let ca2 = ca.clone();
        let mut third = tokio::spawn(async move { ca2.send_frame(Lane::State, frame(3)).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut third)
                .await
                .is_err(),
            "sender should be blocked"
        );
        // Other lanes are independent.
        ca.send_frame(Lane::Bulk, frame(9)).await.unwrap();
        // Reading frees a slot and releases the sender.
        let first = cb.recv().await.unwrap();
        assert_eq!(
            first,
            Received::Frame {
                lane: Lane::State,
                bytes: frame(1)
            }
        );
        third.await.unwrap().unwrap();

        // Datagrams: 30-byte queue, 10-byte datagrams, third overflows and drops the oldest.
        assert_eq!(ca.send_datagram(vec![1; 10]), DatagramSend::Queued);
        assert_eq!(ca.send_datagram(vec![2; 10]), DatagramSend::Queued);
        assert_eq!(ca.send_datagram(vec![3; 10]), DatagramSend::Queued);
        assert_eq!(ca.datagram_space(), 0);
        assert_eq!(
            ca.send_datagram(vec![4; 10]),
            DatagramSend::QueuedDroppedOld
        );
        assert_eq!(ca.send_datagram(vec![0; 21]), DatagramSend::TooLarge);
        let s = ca.stats();
        assert_eq!((s.datagrams_dropped_out, s.datagrams_too_large), (1, 1));
        // Datagrams are delivered ahead of lane 1 frames; the oldest is gone.
        assert_eq!(cb.recv().await, Some(Received::Datagram(vec![2; 10])));
    }

    #[tokio::test]
    async fn cancellation_is_whole_frame_or_nothing() {
        let net = MemNetwork::with_config(MemConfig {
            lane_cap: 1,
            ..Default::default()
        });
        let (_a, _b, ca, cb) = pair(&net).await;
        ca.send_frame(Lane::State, frame(1)).await.unwrap();
        // Lane full: this send is cancelled while waiting, so frame 2 must vanish entirely.
        let r = tokio::time::timeout(
            Duration::from_millis(30),
            ca.send_frame(Lane::State, frame(2)),
        )
        .await;
        assert!(r.is_err());
        // Cancelled sends that did complete are whole: poll-once then drop.
        {
            let fut = ca.send_frame(Lane::Control, frame(7));
            drop(fut); // never polled: nothing queued
        }
        ca.send_frame(Lane::Control, frame(8)).await.unwrap();
        let mut got = vec![];
        for _ in 0..2 {
            match cb.recv().await.unwrap() {
                Received::Frame { bytes, .. } => got.push(bytes),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(got, vec![frame(8), frame(1)]);
        // Frame 2 and 7 never show up.
        assert!(tokio::time::timeout(Duration::from_millis(30), cb.recv())
            .await
            .is_err());
        // The stream is not corrupted: a later frame arrives intact.
        ca.send_frame(Lane::State, frame(3)).await.unwrap();
        assert_eq!(
            cb.recv().await,
            Some(Received::Frame {
                lane: Lane::State,
                bytes: frame(3)
            })
        );
    }

    #[tokio::test]
    async fn priority_lane_zero_first() {
        let net = MemNetwork::new();
        let (_a, _b, ca, cb) = pair(&net).await;
        for i in 0..3 {
            ca.send_frame(Lane::Bulk, frame(20 + i)).await.unwrap();
            ca.send_frame(Lane::State, frame(10 + i)).await.unwrap();
        }
        ca.send_frame(Lane::Control, frame(1)).await.unwrap();
        ca.send_frame(Lane::Control, frame(2)).await.unwrap();
        let mut order = vec![];
        for _ in 0..8 {
            if let Some(Received::Frame { bytes, .. }) = cb.recv().await {
                order.push(bytes[4]);
            }
        }
        assert_eq!(order, vec![1, 2, 10, 11, 12, 20, 21, 22]);
    }
}
