//! Peer frames and datagram header (CONTRACTS.md sections 4–5; PROVISIONAL until R0).

use crate::ids::{Key, RoomId};
use crate::wire::{DecodeError, Reader, Writer};

pub const ALPN: &[u8] = b"prim/1";
pub const HELLO_MAGIC: [u8; 4] = *b"prim";
pub const PROTOCOL_VERSION: u16 = 1;
pub const FRAME_CAP: usize = 1 << 20;
pub const RECORD_CAP: usize = 4096 + 512;
pub const PROOF_CAP: usize = 4;

pub mod caps {
    pub const STATE: u64 = 1 << 0;
    pub const MESSAGES: u64 = 1 << 1;
    pub const DATAGRAMS: u64 = 1 << 2;
    pub const STREAMS: u64 = 1 << 3;
    /// Reserved for round R0.
    pub const VOICE: u64 = 1 << 8;
    pub const R1: u64 = STATE | MESSAGES | DATAGRAMS;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum Reason {
    Version = 1,
    WrongRoom = 2,
    NotMember = 3,
    Closed = 4,
    Revoked = 5,
    Duplicate = 6,
    InviteInvalid = 7,
    Shutdown = 8,
    Leave = 9,
    /// Not on the wire list yet; decoded from any unknown code.
    Other = 0xffff,
}

impl Reason {
    pub fn from_u16(v: u16) -> Reason {
        use Reason::*;
        match v {
            1 => Version,
            2 => WrongRoom,
            3 => NotMember,
            4 => Closed,
            5 => Revoked,
            6 => Duplicate,
            7 => InviteInvalid,
            8 => Shutdown,
            9 => Leave,
            _ => Other,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Target {
    Receiver = 0,
    Broadcast = 1,
    Coordinator = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum AckStatus {
    Delivered = 0,
    NoChannel = 1,
    NotMember = 2,
    TooLarge = 3,
}

pub mod msg_flags {
    pub const RELIABLE: u8 = 1;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    pub version: u16,
    pub caps: u64,
    pub room: RoomId,
    pub member: Key,
    pub daemon_incarnation: u64,
    pub open_generation: u32,
    pub proof: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    Hello(Hello),
    Reject {
        reason: Reason,
        detail: Vec<u8>,
    },
    Records(Vec<Vec<u8>>),
    SnapshotBegin {
        count: u32,
    },
    State(Vec<u8>),
    SnapshotEnd,
    Message {
        msg_id: u64,
        channel: u16,
        flags: u8,
        target: Target,
        payload: Vec<u8>,
    },
    MessageAck {
        msg_id: u64,
        status: AckStatus,
    },
    Goodbye {
        reason: Reason,
    },
}

impl Frame {
    /// Encode with the `u32` length prefix.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(0);
        match self {
            Frame::Hello(h) => {
                w.u8(0x01)
                    .raw(&HELLO_MAGIC)
                    .u16(h.version)
                    .u64(h.caps)
                    .raw(&h.room.0)
                    .raw(&h.member.0);
                w.u64(h.daemon_incarnation)
                    .u32(h.open_generation)
                    .u8(h.proof.len() as u8);
                for p in &h.proof {
                    w.bytes16(p);
                }
            }
            Frame::Reject { reason, detail } => {
                w.u8(0x02).u16(*reason as u16).bytes16(detail);
            }
            Frame::Records(rs) => {
                w.u8(0x10).u16(rs.len() as u16);
                for r in rs {
                    w.bytes16(r);
                }
            }
            Frame::SnapshotBegin { count } => {
                w.u8(0x11).u32(*count);
            }
            Frame::State(b) => {
                w.u8(0x12).bytes16(b);
            }
            Frame::SnapshotEnd => {
                w.u8(0x13);
            }
            Frame::Message {
                msg_id,
                channel,
                flags,
                target,
                payload,
            } => {
                w.u8(0x20)
                    .u64(*msg_id)
                    .u16(*channel)
                    .u8(*flags)
                    .u8(*target as u8)
                    .bytes32(payload);
            }
            Frame::MessageAck { msg_id, status } => {
                w.u8(0x21).u64(*msg_id).u8(*status as u8);
            }
            Frame::Goodbye { reason } => {
                w.u8(0x30).u16(*reason as u16);
            }
        }
        let mut v = w.finish();
        let len = (v.len() - 4) as u32;
        v[..4].copy_from_slice(&len.to_le_bytes());
        v
    }

    /// Decode one frame body (after the length prefix was consumed).
    pub fn decode_body(body: &[u8]) -> Result<Frame, DecodeError> {
        if body.len() > FRAME_CAP {
            return Err(DecodeError::TooLarge("frame"));
        }
        let mut r = Reader::new(body);
        let t = r.u8()?;
        let f = match t {
            0x01 => {
                if r.array::<4>()? != HELLO_MAGIC {
                    return Err(DecodeError::Magic);
                }
                let version = r.u16()?;
                let caps = r.u64()?;
                let room = RoomId(r.array()?);
                let member = Key(r.array()?);
                let daemon_incarnation = r.u64()?;
                let open_generation = r.u32()?;
                let n = r.u8()? as usize;
                if n > PROOF_CAP {
                    return Err(DecodeError::TooLarge("proof"));
                }
                let mut proof = Vec::with_capacity(n);
                for _ in 0..n {
                    proof.push(r.bytes16(RECORD_CAP, "proof record")?.to_vec());
                }
                Frame::Hello(Hello {
                    version,
                    caps,
                    room,
                    member,
                    daemon_incarnation,
                    open_generation,
                    proof,
                })
            }
            0x02 => Frame::Reject {
                reason: Reason::from_u16(r.u16()?),
                detail: r.bytes16(1024, "detail")?.to_vec(),
            },
            0x10 => {
                let n = r.u16()? as usize;
                let mut rs = Vec::with_capacity(n.min(256));
                for _ in 0..n {
                    rs.push(r.bytes16(RECORD_CAP, "record")?.to_vec());
                }
                Frame::Records(rs)
            }
            0x11 => Frame::SnapshotBegin { count: r.u32()? },
            0x12 => Frame::State(r.bytes16(RECORD_CAP, "state")?.to_vec()),
            0x13 => Frame::SnapshotEnd,
            0x20 => {
                let msg_id = r.u64()?;
                let channel = r.u16()?;
                let flags = r.u8()?;
                if flags & !msg_flags::RELIABLE != 0 {
                    return Err(DecodeError::Field("message flags"));
                }
                let target = match r.u8()? {
                    0 => Target::Receiver,
                    1 => Target::Broadcast,
                    2 => Target::Coordinator,
                    _ => return Err(DecodeError::Field("target")),
                };
                let payload = r.bytes32(FRAME_CAP, "payload")?.to_vec();
                Frame::Message {
                    msg_id,
                    channel,
                    flags,
                    target,
                    payload,
                }
            }
            0x21 => {
                let msg_id = r.u64()?;
                let status = match r.u8()? {
                    0 => AckStatus::Delivered,
                    1 => AckStatus::NoChannel,
                    2 => AckStatus::NotMember,
                    3 => AckStatus::TooLarge,
                    _ => return Err(DecodeError::Field("ack status")),
                };
                Frame::MessageAck { msg_id, status }
            }
            0x30 => Frame::Goodbye {
                reason: Reason::from_u16(r.u16()?),
            },
            k => return Err(DecodeError::Kind(k)),
        };
        r.end()?;
        Ok(f)
    }

    /// Decode a whole length-prefixed frame.
    pub fn decode(bytes: &[u8]) -> Result<Frame, DecodeError> {
        let mut r = Reader::new(bytes);
        let len = r.u32()? as usize;
        if len > FRAME_CAP {
            return Err(DecodeError::TooLarge("frame"));
        }
        let body = r.take(len)?;
        r.end()?;
        Frame::decode_body(body)
    }
}

/// Incremental splitter for a byte stream of length-prefixed frames.
#[derive(Default)]
pub struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }
    /// Next complete frame, `Ok(None)` if more bytes are needed.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, DecodeError> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_le_bytes(self.buf[..4].try_into().unwrap()) as usize;
        if len > FRAME_CAP {
            return Err(DecodeError::TooLarge("frame"));
        }
        if self.buf.len() < 4 + len {
            return Ok(None);
        }
        let f = Frame::decode_body(&self.buf[4..4 + len]);
        self.buf.drain(..4 + len);
        f.map(Some)
    }
}

pub const DATAGRAM_VERSION: u8 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Datagram {
    /// An unsigned live state record.
    LiveState(Vec<u8>),
    App {
        channel: u16,
        seq: u16,
        send_ms: u32,
        payload: Vec<u8>,
    },
}

impl Datagram {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        match self {
            Datagram::LiveState(b) => {
                w.u8(DATAGRAM_VERSION << 4).raw(b);
            }
            Datagram::App {
                channel,
                seq,
                send_ms,
                payload,
            } => {
                w.u8((DATAGRAM_VERSION << 4) | 1)
                    .u16(*channel)
                    .u16(*seq)
                    .u32(*send_ms)
                    .raw(payload);
            }
        }
        w.finish()
    }

    pub fn decode(b: &[u8]) -> Result<Datagram, DecodeError> {
        let mut r = Reader::new(b);
        let vk = r.u8()?;
        if vk >> 4 != DATAGRAM_VERSION {
            return Err(DecodeError::Version(vk >> 4));
        }
        match vk & 0xf {
            0 => Ok(Datagram::LiveState(r.take(r.remaining())?.to_vec())),
            1 => {
                let channel = r.u16()?;
                let seq = r.u16()?;
                let send_ms = r.u32()?;
                Ok(Datagram::App {
                    channel,
                    seq,
                    send_ms,
                    payload: r.take(r.remaining())?.to_vec(),
                })
            }
            k => Err(DecodeError::Kind(k)),
        }
    }
}
