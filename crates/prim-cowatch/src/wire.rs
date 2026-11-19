//! Wire formats (version 1). Integers are little-endian and fixed width, decoders are
//! strict (exact lengths, reserved bits zero, unknown kinds rejected), like
//! `docs/room/CONTRACTS.md` section 0.
//!
//! | what | where |
//! |---|---|
//! | intent record | durable state `room/playback` (coordinator mode) or `member/<me>/playback` (acting mode) |
//! | capability record | durable state `member/<me>/cowatch` |
//! | requests and replies | app message channel [`CH_CONTROL`] (reliable) |
//! | clock probes | app datagram channel [`CH_PROBE`] |

pub type Key = [u8; 32];

/// App message channel carrying requests and replies. (Room chat uses 100.)
pub const CH_CONTROL: u16 = 101;
/// App datagram channel carrying clock probes.
pub const CH_PROBE: u16 = 102;

pub const NAME_PLAYBACK: &str = "playback";
pub const NAME_CAP: &str = "cowatch";

/// Longest source string. A durable record may hold 4096 B; the rest is header.
pub const MAX_SOURCE: usize = 3000;
const VER: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    Short,
    Trailing,
    Version,
    Kind,
    Field(&'static str),
}

struct R<'a>(&'a [u8]);
impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.0.len() < n {
            return Err(DecodeError::Short);
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn key(&mut self) -> Result<Key, DecodeError> {
        Ok(self.take(32)?.try_into().unwrap())
    }
    fn bytes16(&mut self) -> Result<Vec<u8>, DecodeError> {
        let n = self.u16()? as usize;
        if n > MAX_SOURCE {
            return Err(DecodeError::Field("source length"));
        }
        Ok(self.take(n)?.to_vec())
    }
    fn end(&self) -> Result<(), DecodeError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(DecodeError::Trailing)
        }
    }
}

fn put_bytes16(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&(b.len() as u16).to_le_bytes());
    out.extend_from_slice(b);
}

/// What the source string means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SourceKind {
    None = 0,
    Url = 1,
    /// A file name every member must have locally (no transfer in this crate).
    File = 2,
}

/// The coordinator epoch plus holder: the key every request, estimate and probe carries
/// (CONTRACTS.md section 2: "apps key on (generation, term, holder)").
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct AuthKey {
    pub generation: u32,
    pub term: u32,
    pub holder: Key,
}

impl AuthKey {
    fn put(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.generation.to_le_bytes());
        out.extend_from_slice(&self.term.to_le_bytes());
        out.extend_from_slice(&self.holder);
    }
    fn get(r: &mut R) -> Result<Self, DecodeError> {
        Ok(Self {
            generation: r.u32()?,
            term: r.u32()?,
            holder: r.key()?,
        })
    }
}

// ---------------------------------------------------------------------------
// Intent record
// ---------------------------------------------------------------------------

/// The durable playback intent. The position at `ref_us` is `position_ms`; the
/// reference time is in the *writer's* cowatch clock (microseconds, any origin).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Intent {
    /// Revision, strictly increasing across writers: a new writer starts above the
    /// highest it has seen.
    pub rev: u32,
    pub paused: bool,
    /// Live source: no seeking and no sync, everyone follows the live edge.
    pub live: bool,
    /// Playback rate in thousandths (1000 = 1.0).
    pub rate_milli: u16,
    pub position_ms: u64,
    pub ref_us: u64,
    /// 0 when unknown.
    pub duration_ms: u64,
    pub kind: SourceKind,
    pub source: Vec<u8>,
}

impl Intent {
    pub fn encode(&self) -> Vec<u8> {
        let mut o = vec![VER];
        o.extend_from_slice(&self.rev.to_le_bytes());
        o.push(self.paused as u8 | (self.live as u8) << 1);
        o.extend_from_slice(&self.rate_milli.to_le_bytes());
        o.extend_from_slice(&self.position_ms.to_le_bytes());
        o.extend_from_slice(&self.ref_us.to_le_bytes());
        o.extend_from_slice(&self.duration_ms.to_le_bytes());
        o.push(self.kind as u8);
        put_bytes16(&mut o, &self.source);
        o
    }

    pub fn decode(b: &[u8]) -> Result<Self, DecodeError> {
        let mut r = R(b);
        if r.u8()? != VER {
            return Err(DecodeError::Version);
        }
        let rev = r.u32()?;
        let flags = r.u8()?;
        if flags & !3 != 0 {
            return Err(DecodeError::Field("flags"));
        }
        let rate_milli = r.u16()?;
        if rate_milli == 0 || rate_milli > 4000 {
            return Err(DecodeError::Field("rate"));
        }
        let position_ms = r.u64()?;
        let ref_us = r.u64()?;
        let duration_ms = r.u64()?;
        let kind = match r.u8()? {
            0 => SourceKind::None,
            1 => SourceKind::Url,
            2 => SourceKind::File,
            _ => return Err(DecodeError::Field("source kind")),
        };
        let source = r.bytes16()?;
        r.end()?;
        if (kind == SourceKind::None) != source.is_empty() {
            return Err(DecodeError::Field("source/kind"));
        }
        Ok(Self {
            rev,
            paused: flags & 1 != 0,
            live: flags & 2 != 0,
            rate_milli,
            position_ms,
            ref_us,
            duration_ms,
            kind,
            source,
        })
    }
}

/// `member/<me>/cowatch`: "I run a player". `since_ms` ranks acting leaders (lowest wins).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cap {
    pub since_ms: u64,
}

impl Cap {
    pub fn encode(&self) -> Vec<u8> {
        let mut o = vec![VER];
        o.extend_from_slice(&self.since_ms.to_le_bytes());
        o.push(0);
        o
    }
    pub fn decode(b: &[u8]) -> Result<Self, DecodeError> {
        let mut r = R(b);
        if r.u8()? != VER {
            return Err(DecodeError::Version);
        }
        let since_ms = r.u64()?;
        if r.u8()? != 0 {
            return Err(DecodeError::Field("flags"));
        }
        r.end()?;
        Ok(Self { since_ms })
    }
}

// ---------------------------------------------------------------------------
// Control channel (reliable messages)
// ---------------------------------------------------------------------------

/// What a request asks for. Absolute, never relative, so a retry is idempotent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Body {
    /// Load this source (and start at `seek_ms`, default 0, playing unless `paused`).
    pub source: Option<(SourceKind, Vec<u8>)>,
    pub paused: Option<bool>,
    pub seek_ms: Option<u64>,
}

impl Body {
    pub fn is_empty(&self) -> bool {
        self.source.is_none() && self.paused.is_none() && self.seek_ms.is_none()
    }
    /// A later body overrides fields of an earlier one; a new source discards an
    /// earlier seek (it belonged to the previous media).
    pub fn merge(&mut self, later: Body) {
        if later.source.is_some() {
            self.source = later.source;
            self.paused = None;
            self.seek_ms = None;
        }
        if later.paused.is_some() {
            self.paused = later.paused;
        }
        if later.seek_ms.is_some() {
            self.seek_ms = later.seek_ms;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ReplyStatus {
    Applied = 0,
    /// The receiver is not the authority the request named.
    WrongAuthority = 1,
    BadRequest = 2,
    /// `req_id` is older than one already applied from this sender.
    Stale = 3,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Control {
    Request {
        req_id: u32,
        key: AuthKey,
        body: Body,
    },
    Reply {
        req_id: u32,
        status: ReplyStatus,
        rev: u32,
    },
}

impl Control {
    pub fn encode(&self) -> Vec<u8> {
        let mut o = Vec::new();
        match self {
            Control::Request { req_id, key, body } => {
                o.push(VER << 4 | 1);
                o.extend_from_slice(&req_id.to_le_bytes());
                key.put(&mut o);
                let flags = u8::from(body.source.is_some())
                    | (u8::from(body.paused.is_some()) << 1)
                    | (u8::from(body.paused == Some(true)) << 2)
                    | (u8::from(body.seek_ms.is_some()) << 3);
                o.push(flags);
                if let Some(s) = body.seek_ms {
                    o.extend_from_slice(&s.to_le_bytes());
                }
                if let Some((k, s)) = &body.source {
                    o.push(*k as u8);
                    put_bytes16(&mut o, s);
                }
            }
            Control::Reply {
                req_id,
                status,
                rev,
            } => {
                o.push(VER << 4 | 2);
                o.extend_from_slice(&req_id.to_le_bytes());
                o.push(*status as u8);
                o.extend_from_slice(&rev.to_le_bytes());
            }
        }
        o
    }

    pub fn decode(b: &[u8]) -> Result<Self, DecodeError> {
        let mut r = R(b);
        let h = r.u8()?;
        if h >> 4 != VER {
            return Err(DecodeError::Version);
        }
        let v = match h & 15 {
            1 => {
                let req_id = r.u32()?;
                let key = AuthKey::get(&mut r)?;
                let flags = r.u8()?;
                if flags & !0xf != 0 || (flags & 4 != 0 && flags & 2 == 0) {
                    return Err(DecodeError::Field("flags"));
                }
                let seek_ms = if flags & 8 != 0 { Some(r.u64()?) } else { None };
                let source = if flags & 1 != 0 {
                    let kind = match r.u8()? {
                        1 => SourceKind::Url,
                        2 => SourceKind::File,
                        _ => return Err(DecodeError::Field("source kind")),
                    };
                    let s = r.bytes16()?;
                    if s.is_empty() {
                        return Err(DecodeError::Field("source"));
                    }
                    Some((kind, s))
                } else {
                    None
                };
                let paused = (flags & 2 != 0).then_some(flags & 4 != 0);
                Control::Request {
                    req_id,
                    key,
                    body: Body {
                        source,
                        paused,
                        seek_ms,
                    },
                }
            }
            2 => {
                let req_id = r.u32()?;
                let status = match r.u8()? {
                    0 => ReplyStatus::Applied,
                    1 => ReplyStatus::WrongAuthority,
                    2 => ReplyStatus::BadRequest,
                    3 => ReplyStatus::Stale,
                    _ => return Err(DecodeError::Field("status")),
                };
                Control::Reply {
                    req_id,
                    status,
                    rev: r.u32()?,
                }
            }
            _ => return Err(DecodeError::Kind),
        };
        r.end()?;
        Ok(v)
    }
}

// ---------------------------------------------------------------------------
// Probe channel (datagrams)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    /// `t1_us` is the requester's clock at send.
    Ping { id: u32, key: AuthKey, t1_us: u64 },
    /// Echo of the ping plus the authority's clock at receive (`t2`) and at send (`t3`).
    Pong {
        id: u32,
        key: AuthKey,
        t1_us: u64,
        t2_us: u64,
        t3_us: u64,
    },
}

impl Probe {
    pub fn encode(&self) -> Vec<u8> {
        let mut o = Vec::new();
        match self {
            Probe::Ping { id, key, t1_us } => {
                o.push(VER << 4 | 1);
                o.extend_from_slice(&id.to_le_bytes());
                key.put(&mut o);
                o.extend_from_slice(&t1_us.to_le_bytes());
            }
            Probe::Pong {
                id,
                key,
                t1_us,
                t2_us,
                t3_us,
            } => {
                o.push(VER << 4 | 2);
                o.extend_from_slice(&id.to_le_bytes());
                key.put(&mut o);
                for t in [t1_us, t2_us, t3_us] {
                    o.extend_from_slice(&t.to_le_bytes());
                }
            }
        }
        o
    }

    pub fn decode(b: &[u8]) -> Result<Self, DecodeError> {
        let mut r = R(b);
        let h = r.u8()?;
        if h >> 4 != VER {
            return Err(DecodeError::Version);
        }
        let v = match h & 15 {
            1 => Probe::Ping {
                id: r.u32()?,
                key: AuthKey::get(&mut r)?,
                t1_us: r.u64()?,
            },
            2 => Probe::Pong {
                id: r.u32()?,
                key: AuthKey::get(&mut r)?,
                t1_us: r.u64()?,
                t2_us: r.u64()?,
                t3_us: r.u64()?,
            },
            _ => return Err(DecodeError::Kind),
        };
        r.end()?;
        Ok(v)
    }
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
