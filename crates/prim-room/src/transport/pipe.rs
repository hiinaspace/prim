//! One direction of a connection as seen by its receiver: three bounded frame queues
//! and a bounded datagram queue, with strict lane priority on the way out.
//!
//! Both transports use it. `mem` pushes into the peer's pipe directly; `iroh` fills
//! its own inbox from stream reader tasks.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use room_core::api::Lane;
use tokio::sync::Notify;

use super::{lane_from_index, ConnClosed, DatagramSend, Received};

struct Inner {
    lanes: [VecDeque<Vec<u8>>; 3],
    dgrams: VecDeque<Vec<u8>>,
    dgram_bytes: usize,
    closed: bool,
}

pub(crate) struct Pipe {
    inner: Mutex<Inner>,
    lane_cap: usize,
    dgram_cap_bytes: usize,
    /// Woken when something is pushed or the pipe closes.
    readable: Notify,
    /// Woken when a frame is popped or the pipe closes.
    writable: Notify,
    pub dgram_dropped: AtomicU64,
}

impl Pipe {
    pub fn new(lane_cap: usize, dgram_cap_bytes: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                lanes: Default::default(),
                dgrams: VecDeque::new(),
                dgram_bytes: 0,
                closed: false,
            }),
            lane_cap,
            dgram_cap_bytes,
            readable: Notify::new(),
            writable: Notify::new(),
            dgram_dropped: AtomicU64::new(0),
        }
    }

    /// Wait for lane room, then queue the whole frame. Cancel-safe: the push is one
    /// step under the lock.
    pub async fn push_frame(&self, lane: Lane, bytes: Vec<u8>) -> Result<(), ConnClosed> {
        let idx = lane as usize;
        loop {
            let notified = self.writable.notified();
            tokio::pin!(notified);
            // Register before checking so a pop between check and await isn't lost.
            notified.as_mut().enable();
            {
                let mut g = self.inner.lock().unwrap();
                if g.closed {
                    return Err(ConnClosed);
                }
                if g.lanes[idx].len() < self.lane_cap {
                    g.lanes[idx].push_back(bytes);
                    drop(g);
                    self.readable.notify_one();
                    return Ok(());
                }
            }
            notified.await;
        }
    }

    /// Queue a datagram, dropping the oldest ones if it would exceed the byte cap.
    pub fn push_datagram(&self, bytes: Vec<u8>) -> DatagramSend {
        let mut dropped = false;
        {
            let mut g = self.inner.lock().unwrap();
            if g.closed {
                return DatagramSend::Closed;
            }
            if bytes.len() > self.dgram_cap_bytes {
                return DatagramSend::TooLarge;
            }
            while g.dgram_bytes + bytes.len() > self.dgram_cap_bytes {
                if let Some(old) = g.dgrams.pop_front() {
                    g.dgram_bytes -= old.len();
                    dropped = true;
                    self.dgram_dropped.fetch_add(1, Ordering::Relaxed);
                } else {
                    break;
                }
            }
            g.dgram_bytes += bytes.len();
            g.dgrams.push_back(bytes);
        }
        self.readable.notify_one();
        if dropped {
            DatagramSend::QueuedDroppedOld
        } else {
            DatagramSend::Queued
        }
    }

    pub fn datagram_space(&self) -> usize {
        let g = self.inner.lock().unwrap();
        self.dgram_cap_bytes.saturating_sub(g.dgram_bytes)
    }

    /// Priority order: lane 0, datagrams, lane 1, lane 2.
    fn pop(&self, g: &mut Inner) -> Option<Received> {
        if let Some(b) = g.lanes[0].pop_front() {
            return Some(Received::Frame {
                lane: Lane::Control,
                bytes: b,
            });
        }
        if let Some(d) = g.dgrams.pop_front() {
            g.dgram_bytes -= d.len();
            return Some(Received::Datagram(d));
        }
        for i in 1..3 {
            if let Some(b) = g.lanes[i].pop_front() {
                return Some(Received::Frame {
                    lane: lane_from_index(i),
                    bytes: b,
                });
            }
        }
        None
    }

    /// Next item by priority. `None` once closed and drained.
    pub async fn recv(&self) -> Option<Received> {
        loop {
            let notified = self.readable.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut g = self.inner.lock().unwrap();
                if let Some(item) = self.pop(&mut g) {
                    drop(g);
                    self.writable.notify_waiters();
                    // Another reader-side wakeup in case more is queued.
                    self.readable.notify_one();
                    return Some(item);
                }
                if g.closed {
                    return None;
                }
            }
            notified.await;
        }
    }

    /// Stop accepting pushes. Items already queued stay deliverable.
    pub fn close(&self) {
        self.inner.lock().unwrap().closed = true;
        self.readable.notify_one();
        self.writable.notify_waiters();
    }

    pub fn dropped(&self) -> u64 {
        self.dgram_dropped.load(Ordering::Relaxed)
    }
}
