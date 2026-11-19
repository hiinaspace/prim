//! The client: one worker thread owns reading and reconnecting; any thread may issue
//! requests. Events arrive on a channel the owner polls (Godot polls it from `_process`
//! and emits signals on the main thread).
//!
//! Reconnect: when the socket drops (daemon restart, or the daemon detached a slow
//! client) the worker emits `Disconnected`, retries with backoff, and on reconnect
//! re-subscribes every pattern and re-registers every channel with the same ids, then
//! emits `Connected`. Subscriptions therefore deliver a fresh `Reset` + snapshot after
//! each reconnect; consumers must treat a `Connected` as "state restarts".
//! Writes, sends and status calls made while disconnected fail with `NotConnected`:
//! they are not queued (stale pose writes are worthless; callers retry what matters).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{Error, ErrorKind, Result};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::transport::{Conn, Transport};
use crate::wire::{self, b64, Event, ReplyJson, Request};

#[derive(Debug, Clone, PartialEq)]
pub enum ClientEvent {
    /// Socket up; subscriptions and channels were replayed.
    Connected,
    /// Socket lost; the worker is retrying.
    Disconnected,
    Event(Event),
    /// A message this client version does not know (kept for forward compatibility).
    Unknown(serde_json::Value),
}

#[derive(Default)]
struct Registry {
    subs: BTreeMap<u32, String>,
    channels: BTreeSet<u16>,
}

struct Inner {
    transport: Box<dyn Transport>,
    writer: Mutex<Option<Box<dyn Conn>>>,
    registry: Mutex<Registry>,
    next_req: AtomicU64,
    next_sub: AtomicU32,
    waiters: Mutex<HashMap<u64, Sender<ReplyJson>>>,
    /// Request ids of replayed requests whose replies nobody wants.
    swallow: Mutex<HashSet<u64>>,
    stop: AtomicBool,
    connected: AtomicBool,
    tx: Sender<ClientEvent>,
}

pub struct Client {
    inner: Arc<Inner>,
    rx: Receiver<ClientEvent>,
    worker: Option<JoinHandle<()>>,
}

fn not_connected() -> Error {
    Error::new(ErrorKind::NotConnected, "not connected to the room daemon")
}

impl Inner {
    fn send_locked(&self, req: &Request) -> Result<()> {
        let bytes = wire::frame(req)?;
        let mut g = self.writer.lock().unwrap();
        let Some(conn) = g.as_mut() else {
            return Err(not_connected());
        };
        use std::io::Write;
        if let Err(e) = conn.write_all(&bytes).and_then(|_| conn.flush()) {
            conn.shutdown();
            *g = None;
            return Err(e);
        }
        Ok(())
    }

    fn req(&self) -> u64 {
        self.next_req.fetch_add(1, Ordering::Relaxed)
    }

    fn emit(&self, e: ClientEvent) {
        let _ = self.tx.send(e);
    }

    fn dispatch(&self, body: &[u8]) {
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
            return;
        };
        match serde_json::from_value::<Event>(v.clone()) {
            Ok(Event::Reply { req, reply }) => {
                // Replayed requests: swallow the Ok, surface a failure.
                if self.swallow.lock().unwrap().remove(&req) && reply == ReplyJson::Ok {
                    return;
                }
                if let Some(w) = self.waiters.lock().unwrap().remove(&req) {
                    let _ = w.send(reply);
                } else {
                    self.emit(ClientEvent::Event(Event::Reply { req, reply }));
                }
            }
            Ok(e) => self.emit(ClientEvent::Event(e)),
            Err(_) => self.emit(ClientEvent::Unknown(v)),
        }
    }

    /// Sleep up to `d`, waking early when stopped.
    fn nap(&self, d: Duration) {
        let step = Duration::from_millis(20);
        let mut left = d;
        while !left.is_zero() && !self.stop.load(Ordering::Relaxed) {
            let s = step.min(left);
            std::thread::sleep(s);
            left -= s;
        }
    }

    fn run(self: Arc<Self>) {
        let mut backoff = Duration::from_millis(50);
        while !self.stop.load(Ordering::Relaxed) {
            let conn = match self.transport.connect() {
                Ok(c) => c,
                Err(_) => {
                    self.nap(backoff);
                    backoff = (backoff * 2).min(Duration::from_millis(500));
                    continue;
                }
            };
            let Ok(mut reader) = conn.try_clone_box() else {
                self.nap(backoff);
                continue;
            };
            backoff = Duration::from_millis(50);
            {
                // Hold the registry across publishing the writer and replaying, so a
                // concurrent subscribe() is sent exactly once.
                let reg = self.registry.lock().unwrap();
                if self.stop.load(Ordering::Relaxed) {
                    // close() ran before the writer was published: nothing would shut
                    // this connection down and the read below would block forever.
                    conn.shutdown();
                    break;
                }
                *self.writer.lock().unwrap() = Some(conn);
                for (sub, pattern) in &reg.subs {
                    let req = self.req();
                    self.swallow.lock().unwrap().insert(req);
                    let _ = self.send_locked(&Request::Subscribe {
                        req,
                        sub: *sub,
                        pattern: pattern.clone(),
                    });
                }
                for ch in &reg.channels {
                    let req = self.req();
                    self.swallow.lock().unwrap().insert(req);
                    let _ = self.send_locked(&Request::RegisterChannel { req, channel: *ch });
                }
            }
            self.connected.store(true, Ordering::Relaxed);
            self.emit(ClientEvent::Connected);
            loop {
                match wire::read_raw(&mut reader) {
                    Ok(Some(body)) => self.dispatch(&body),
                    Ok(None) | Err(_) => break,
                }
            }
            if let Some(c) = self.writer.lock().unwrap().take() {
                c.shutdown();
            }
            self.connected.store(false, Ordering::Relaxed);
            // Anyone blocked in call() gets a closed channel and returns an error.
            self.waiters.lock().unwrap().clear();
            if !self.stop.load(Ordering::Relaxed) {
                self.emit(ClientEvent::Disconnected);
            }
        }
    }
}

impl Client {
    /// Start the worker; connects (and keeps reconnecting) in the background.
    pub fn start(transport: impl Transport) -> Client {
        let (tx, rx) = channel();
        let inner = Arc::new(Inner {
            transport: Box::new(transport),
            writer: Mutex::new(None),
            registry: Mutex::new(Registry::default()),
            next_req: AtomicU64::new(1),
            next_sub: AtomicU32::new(1),
            waiters: Mutex::new(HashMap::new()),
            swallow: Mutex::new(HashSet::new()),
            stop: AtomicBool::new(false),
            connected: AtomicBool::new(false),
            tx,
        });
        let w = inner.clone();
        let worker = std::thread::Builder::new()
            .name("prim-client".into())
            .spawn(move || w.run())
            .expect("spawn prim-client thread");
        Client {
            inner,
            rx,
            worker: Some(worker),
        }
    }

    pub fn is_connected(&self) -> bool {
        self.inner.connected.load(Ordering::Relaxed)
    }

    pub fn describe(&self) -> String {
        self.inner.transport.describe()
    }

    /// Next event, if one is waiting.
    pub fn try_recv(&self) -> Option<ClientEvent> {
        self.rx.try_recv().ok()
    }

    pub fn recv_timeout(&self, d: Duration) -> Option<ClientEvent> {
        self.rx.recv_timeout(d).ok()
    }

    /// Subscribe to a state pattern. Returns the subscription id; the snapshot, then
    /// deltas, arrive as `Event::Updates { sub, .. }`. Remembered across reconnects.
    pub fn subscribe(&self, pattern: &str) -> Result<u32> {
        let sub = self.inner.next_sub.fetch_add(1, Ordering::Relaxed);
        let mut reg = self.inner.registry.lock().unwrap();
        reg.subs.insert(sub, pattern.into());
        let req = self.inner.req();
        match self.inner.send_locked(&Request::Subscribe {
            req,
            sub,
            pattern: pattern.into(),
        }) {
            Ok(()) => Ok(sub),
            // Replayed on connect.
            Err(e) if e.kind() == ErrorKind::NotConnected => Ok(sub),
            Err(e) => Err(e),
        }
    }

    pub fn unsubscribe(&self, sub: u32) -> Result<()> {
        self.inner.registry.lock().unwrap().subs.remove(&sub);
        self.inner.send_locked(&Request::Unsubscribe { sub })
    }

    /// Write state. `scope` is "member" or "room"; `live` = overwritten-latest records
    /// (<= 200 B), otherwise durable. Returns the request id; the result is a `Reply`.
    pub fn write(
        &self,
        scope: &str,
        name: &str,
        live: bool,
        persistent: bool,
        value: &[u8],
    ) -> Result<u64> {
        let req = self.inner.req();
        self.inner.send_locked(&Request::Write {
            req,
            scope: scope.into(),
            name: name.into(),
            live,
            persistent,
            value: b64(value),
        })?;
        Ok(req)
    }

    pub fn register_channel(&self, channel: u16) -> Result<u64> {
        let mut reg = self.inner.registry.lock().unwrap();
        reg.channels.insert(channel);
        let req = self.inner.req();
        match self
            .inner
            .send_locked(&Request::RegisterChannel { req, channel })
        {
            Ok(()) => Ok(req),
            Err(e) if e.kind() == ErrorKind::NotConnected => Ok(req),
            Err(e) => Err(e),
        }
    }

    pub fn unregister_channel(&self, channel: u16) -> Result<()> {
        self.inner.registry.lock().unwrap().channels.remove(&channel);
        self.inner
            .send_locked(&Request::UnregisterChannel { channel })
    }

    /// Send an app message to "all", "coordinator" or a member key (hex). Returns the
    /// request id: a `Reply::Sending{msg_id}` follows, then `MessageResult` per target.
    pub fn send(&self, to: &str, channel: u16, reliable: bool, payload: &[u8]) -> Result<u64> {
        let req = self.inner.req();
        self.inner.send_locked(&Request::Send {
            req,
            to: to.into(),
            channel,
            reliable,
            payload: b64(payload),
        })?;
        Ok(req)
    }

    pub fn send_datagram(
        &self,
        to: &str,
        channel: u16,
        payload: &[u8],
        max_age_ms: u32,
    ) -> Result<()> {
        self.inner.send_locked(&Request::SendDatagram {
            to: to.into(),
            channel,
            payload: b64(payload),
            max_age_ms,
        })
    }

    pub fn status(&self) -> Result<u64> {
        let req = self.inner.req();
        self.inner.send_locked(&Request::Status { req })?;
        Ok(req)
    }

    /// Send any request built from a fresh request id and block for its reply.
    pub fn call(
        &self,
        make: impl FnOnce(u64) -> Request,
        timeout: Duration,
    ) -> Result<ReplyJson> {
        let req = self.inner.req();
        let (tx, rx) = channel();
        self.inner.waiters.lock().unwrap().insert(req, tx);
        if let Err(e) = self.inner.send_locked(&make(req)) {
            self.inner.waiters.lock().unwrap().remove(&req);
            return Err(e);
        }
        match rx.recv_timeout(timeout) {
            Ok(r) => Ok(r),
            Err(RecvTimeoutError::Timeout) => {
                self.inner.waiters.lock().unwrap().remove(&req);
                Err(Error::new(ErrorKind::TimedOut, "no reply from the room daemon"))
            }
            Err(RecvTimeoutError::Disconnected) => Err(Error::new(
                ErrorKind::ConnectionAborted,
                "daemon connection lost while waiting",
            )),
        }
    }

    /// Stop the worker and close the socket.
    pub fn close(&mut self) {
        self.inner.stop.store(true, Ordering::Relaxed);
        if let Some(c) = self.inner.writer.lock().unwrap().take() {
            c.shutdown();
        }
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.close();
    }
}
