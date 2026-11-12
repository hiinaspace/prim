//! State-record envelope (CONTRACTS.md section 3).

use crate::grants::Epoch;
use crate::ids::{Identity, Key, RoomId};
use crate::wire::{DecodeError, Reader, Writer};

pub const STATE_MAGIC: [u8; 2] = *b"PS";
pub const STATE_VERSION: u8 = 1;
pub const STATE_SIG_CTX: &[u8] = b"prim/1 state\0";
pub const NAME_CAP: usize = 48;
pub const LIVE_VALUE_CAP: usize = 200;
pub const DURABLE_VALUE_CAP: usize = 4096;
pub const PRESENCE: &str = "presence";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Scope {
    Member = 0,
    Room = 1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Class {
    Durable,
    Live,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lifetime {
    PresenceBound,
    Persistent,
}

/// Version of a record within one (writer, scope, name, class): `(generation, term, seq)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Version {
    pub generation: u32,
    pub term: u32,
    pub seq: u64,
}

impl Version {
    pub fn epoch(&self) -> Epoch {
        Epoch {
            generation: self.generation,
            term: self.term,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateRecord {
    pub class: Class,
    pub lifetime: Lifetime,
    pub scope: Scope,
    pub writer: Key,
    pub version: Version,
    pub name: String,
    pub value: Vec<u8>,
    /// Exact wire bytes (with signature for durable records).
    pub bytes: Vec<u8>,
}

/// Valid name: 1..=48 bytes of `[a-z0-9._-]` segments joined by `/`, no empty segment.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_CAP
        && name.split('/').all(|seg| {
            !seg.is_empty()
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
        })
}

/// Full path as subscribers see it: `member/<writer-hex>/<name>` or `room/<name>`.
pub fn path(scope: Scope, writer: &Key, name: &str) -> String {
    match scope {
        Scope::Member => format!("member/{}/{}", writer.hex(), name),
        Scope::Room => format!("room/{name}"),
    }
}

impl StateRecord {
    pub fn path(&self) -> String {
        path(self.scope, &self.writer, &self.name)
    }

    fn unsigned_bytes(
        class: Class,
        lifetime: Lifetime,
        scope: Scope,
        writer: Key,
        version: Version,
        name: &str,
        value: &[u8],
    ) -> Vec<u8> {
        let mut flags = 0u8;
        if class == Class::Live {
            flags |= 1;
        }
        if lifetime == Lifetime::Persistent {
            flags |= 2;
        }
        let mut w = Writer::new();
        w.raw(&STATE_MAGIC)
            .u8(STATE_VERSION)
            .u8(flags)
            .u8(scope as u8)
            .raw(&writer.0);
        w.u32(version.generation).u32(version.term).u64(version.seq);
        w.bytes8(name.as_bytes()).bytes16(value);
        w.finish()
    }

    /// Build a record. Durable records are signed by `signer` (which must be the writer).
    /// Errors on invalid name or over-cap value.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        signer: &Identity,
        room: RoomId,
        class: Class,
        lifetime: Lifetime,
        scope: Scope,
        version: Version,
        name: &str,
        value: &[u8],
    ) -> Result<StateRecord, DecodeError> {
        if !valid_name(name) {
            return Err(DecodeError::Field("name"));
        }
        let cap = if class == Class::Live {
            LIVE_VALUE_CAP
        } else {
            DURABLE_VALUE_CAP
        };
        if value.len() > cap {
            return Err(DecodeError::TooLarge("value"));
        }
        let writer = signer.key();
        let mut bytes = Self::unsigned_bytes(class, lifetime, scope, writer, version, name, value);
        if class == Class::Durable {
            let sig = signer.sign(&sig_msg(room, &bytes));
            bytes.extend_from_slice(&sig);
        }
        Ok(StateRecord {
            class,
            lifetime,
            scope,
            writer,
            version,
            name: name.to_string(),
            value: value.to_vec(),
            bytes,
        })
    }

    /// Decode and (for durable records) verify against `room`.
    pub fn decode(room: RoomId, bytes: &[u8]) -> Result<StateRecord, DecodeError> {
        let mut r = Reader::new(bytes);
        if r.array::<2>()? != STATE_MAGIC {
            return Err(DecodeError::Magic);
        }
        let v = r.u8()?;
        if v != STATE_VERSION {
            return Err(DecodeError::Version(v));
        }
        let flags = r.u8()?;
        if flags & !3 != 0 {
            return Err(DecodeError::Field("flags"));
        }
        let class = if flags & 1 != 0 {
            Class::Live
        } else {
            Class::Durable
        };
        let lifetime = if flags & 2 != 0 {
            Lifetime::Persistent
        } else {
            Lifetime::PresenceBound
        };
        let scope = match r.u8()? {
            0 => Scope::Member,
            1 => Scope::Room,
            _ => return Err(DecodeError::Field("scope")),
        };
        let writer = Key(r.array()?);
        let version = Version {
            generation: r.u32()?,
            term: r.u32()?,
            seq: r.u64()?,
        };
        let name_bytes = r.bytes8(NAME_CAP, "name")?;
        let name = std::str::from_utf8(name_bytes).map_err(|_| DecodeError::Field("name"))?;
        if !valid_name(name) {
            return Err(DecodeError::Field("name"));
        }
        let cap = if class == Class::Live {
            LIVE_VALUE_CAP
        } else {
            DURABLE_VALUE_CAP
        };
        let value = r.bytes16(cap, "value")?.to_vec();
        let signed_len = r.pos();
        if class == Class::Durable {
            let sig: [u8; 64] = r.array()?;
            r.end()?;
            if !writer.verify(&sig_msg(room, &bytes[..signed_len]), &sig) {
                return Err(DecodeError::Signature);
            }
        } else {
            r.end()?;
        }
        Ok(StateRecord {
            class,
            lifetime,
            scope,
            writer,
            version,
            name: name.to_string(),
            value,
            bytes: bytes.to_vec(),
        })
    }
}

fn sig_msg(room: RoomId, unsigned: &[u8]) -> Vec<u8> {
    let mut m = STATE_SIG_CTX.to_vec();
    m.extend_from_slice(&room.0);
    m.extend_from_slice(unsigned);
    m
}

/// Presence value (CONTRACTS.md section 3, "Presence"). Empty bytes = offline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Presence {
    pub since_ms: u64,
    pub daemon_incarnation: u64,
}

impl Presence {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u8(1)
            .u8(1)
            .u64(self.since_ms)
            .u64(self.daemon_incarnation);
        w.finish()
    }
    /// `Ok(None)` = offline.
    pub fn decode(v: &[u8]) -> Result<Option<Presence>, DecodeError> {
        if v.is_empty() {
            return Ok(None);
        }
        let mut r = Reader::new(v);
        if r.u8()? != 1 {
            return Err(DecodeError::Version(v[0]));
        }
        if r.u8()? != 1 {
            return Err(DecodeError::Field("presence state"));
        }
        let p = Presence {
            since_ms: r.u64()?,
            daemon_incarnation: r.u64()?,
        };
        r.end()?;
        Ok(Some(p))
    }
}
