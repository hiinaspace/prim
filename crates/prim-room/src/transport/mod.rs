//! The seam between the driver and the network.
//!
//! The driver is generic over [`Transport`], so the same code runs on QUIC
//! ([`iroh`]) and in memory ([`mem`]). Both implement the same semantics, which the
//! tests lean on:
//!
//! * **Lifecycle.** A close from either side ends the connection for both: `recv`
//!   returns `None` once everything queued before the close has been delivered, and
//!   `closed()` resolves with the reason. Dropping or shutting down a transport
//!   closes all its connections. Dialling an absent key fails.
//! * **Overload.** Each lane is a bounded frame queue: `send_frame` waits for room.
//!   Datagrams never wait: a full queue drops the oldest and counts it.
//! * **Cancellation.** `send_frame` is cancel-safe: if its future is dropped the whole
//!   frame was queued or none of it was. Frames are only ever written to a stream
//!   whole, by one writer task per lane.
//! * **Priority.** When several lanes have frames waiting, lane 0 goes first. QUIC
//!   applies this when sending (`set_priority`); the receive side of both transports
//!   also hands out waiting lane 0 frames before lane 1 and lane 2 ones.

use std::future::Future;

use room_core::api::Lane;
use room_core::frame::Reason;
use room_core::ids::Key;

pub mod iroh;
pub mod mem;
pub(crate) mod pipe;

/// What a connection hands to its consumer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Received {
    /// One complete frame, length prefix included.
    Frame {
        lane: Lane,
        bytes: Vec<u8>,
    },
    Datagram(Vec<u8>),
}

/// Outcome of a non-blocking datagram send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatagramSend {
    /// Handed to the transport (it may still be lost in flight).
    Queued,
    /// Handed to the transport, and it dropped an older queued datagram to make room.
    QueuedDroppedOld,
    /// Larger than the connection's current `max_datagram_size` (or datagrams are off).
    TooLarge,
    Closed,
}

/// The connection ended (or is ending).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloseInfo {
    /// `Some` when the closing side gave a reason this code understands.
    pub reason: Option<Reason>,
    /// True when this side called `close`.
    pub local: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteKind {
    Direct,
    Relay,
    Mem,
    Unknown,
}

impl RouteKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RouteKind::Direct => "direct",
            RouteKind::Relay => "relay",
            RouteKind::Mem => "mem",
            RouteKind::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    pub kind: RouteKind,
    pub max_datagram_size: Option<usize>,
    pub rtt_ms: Option<u64>,
}

/// Counters of a connection (this side).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConnStats {
    /// Outgoing datagrams the transport dropped to make room for newer ones.
    pub datagrams_dropped_out: u64,
    /// Outgoing datagrams refused as too large.
    pub datagrams_too_large: u64,
    /// Incoming datagrams dropped because the consumer was too slow.
    pub datagrams_dropped_in: u64,
}

#[derive(Debug)]
pub struct TransportError(pub String);

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for TransportError {}

/// A frame could not be sent because the connection is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnClosed;

pub trait Transport: Send + Sync + 'static {
    type Conn: Connection;

    /// This endpoint's member key.
    fn local_key(&self) -> Key;
    /// Open a connection to `peer`. `hints` are relay URLs or `ip:port` direct
    /// addresses (anything else is ignored).
    fn dial(
        &self,
        peer: Key,
        hints: Vec<String>,
    ) -> impl Future<Output = Result<Self::Conn, TransportError>> + Send;
    /// The next inbound connection, `None` once the transport is shut down.
    fn accept(&self) -> impl Future<Output = Option<Self::Conn>> + Send;
    /// Close every connection and stop accepting. Idempotent.
    fn shutdown(&self) -> impl Future<Output = ()> + Send;
}

pub trait Connection: Clone + Send + Sync + 'static {
    /// The authenticated remote key.
    fn peer(&self) -> Key;
    /// True when this side dialled.
    fn outbound(&self) -> bool;
    /// Queue one frame (length prefix included) on `lane`. Waits while the lane's queue
    /// is full. Cancel-safe: the frame is queued whole or not at all.
    fn send_frame(
        &self,
        lane: Lane,
        bytes: Vec<u8>,
    ) -> impl Future<Output = Result<(), ConnClosed>> + Send;
    /// Non-blocking. May drop the oldest queued datagram to make room.
    fn send_datagram(&self, bytes: Vec<u8>) -> DatagramSend;
    /// Bytes of datagram queue space left before an older datagram would be dropped.
    fn datagram_space(&self) -> usize;
    /// Next frame or datagram, lane 0 first. `None` after the connection closed and
    /// everything queued before that was delivered. One consumer at a time.
    fn recv(&self) -> impl Future<Output = Option<Received>> + Send;
    /// Like `recv` but frames only, lane 0 first. Pair it with `recv_datagram` on a
    /// second task (never with `recv`) so datagrams don't wait behind a consumer that
    /// is blocked handing frames to a busy core. `None` after close and drain.
    fn recv_frame(&self) -> impl Future<Output = Option<(Lane, Vec<u8>)>> + Send;
    /// Like `recv` but datagrams only. `None` after close and drain.
    fn recv_datagram(&self) -> impl Future<Output = Option<Vec<u8>>> + Send;
    /// Flush what is queued, then close with `reason`. Returns at once.
    fn close(&self, reason: Reason);
    /// Resolves when the connection is closed.
    fn closed(&self) -> impl Future<Output = CloseInfo> + Send;
    fn route(&self) -> Route;
    fn stats(&self) -> ConnStats;
}

/// Splits a byte stream into whole frames, keeping the raw bytes (the core decodes
/// them). `room_core::frame::FrameReader` decodes eagerly and drops the bytes, so it
/// can't feed `Input::Frame`.
#[derive(Default)]
pub struct RawFrameReader {
    buf: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct FrameTooLarge(pub usize);

impl RawFrameReader {
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, FrameTooLarge> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_le_bytes(self.buf[..4].try_into().unwrap()) as usize;
        if len > room_core::frame::FRAME_CAP {
            return Err(FrameTooLarge(len));
        }
        if self.buf.len() < 4 + len {
            return Ok(None);
        }
        let rest = self.buf.split_off(4 + len);
        Ok(Some(std::mem::replace(&mut self.buf, rest)))
    }
}

pub(crate) fn lane_from_index(i: usize) -> Lane {
    match i {
        0 => Lane::Control,
        1 => Lane::State,
        _ => Lane::Bulk,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_reader_splits_and_keeps_bytes() {
        let mut a = 3u32.to_le_bytes().to_vec();
        a.extend_from_slice(&[1, 2, 3]);
        let mut b = 1u32.to_le_bytes().to_vec();
        b.push(9);
        let mut r = RawFrameReader::default();
        let all: Vec<u8> = a.iter().chain(b.iter()).copied().collect();
        r.push(&all[..5]);
        assert_eq!(r.next_frame(), Ok(None));
        r.push(&all[5..]);
        assert_eq!(r.next_frame(), Ok(Some(a)));
        assert_eq!(r.next_frame(), Ok(Some(b)));
        assert_eq!(r.next_frame(), Ok(None));
    }

    #[test]
    fn raw_reader_passes_records_end_through() {
        use room_core::frame::Frame;
        let bytes = Frame::RecordsEnd.encode();
        let mut r = RawFrameReader::default();
        r.push(&bytes);
        let got = r.next_frame().unwrap().expect("one frame");
        assert_eq!(got, bytes);
        assert_eq!(Frame::decode(&got), Ok(Frame::RecordsEnd));
        assert_eq!(r.next_frame(), Ok(None));
    }

    #[test]
    fn raw_reader_rejects_oversize() {
        let mut r = RawFrameReader::default();
        r.push(&((room_core::frame::FRAME_CAP as u32 + 1).to_le_bytes()));
        assert!(r.next_frame().is_err());
    }
}
