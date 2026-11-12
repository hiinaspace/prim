//! Signed records: genesis, grants, revocations, lifecycle, claims (CONTRACTS.md 1–2).

use crate::ids::{Identity, Key, RecordId, RoomId};
use crate::wire::{h, DecodeError, Reader, Writer};

pub const RECORD_MAGIC: [u8; 2] = *b"PR";
pub const RECORD_VERSION: u8 = 1;
pub const META_CAP: usize = 256;
pub const KEPT_CAP: usize = 64;
pub const RECORD_SIG_CTX: &[u8] = b"prim/1 record\0";
const HEADER: usize = 68;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Kind {
    Genesis = 0,
    Keyholder = 1,
    Inviter = 2,
    Member = 3,
    Invite = 4,
    Revoke = 5,
    Retire = 6,
    Open = 7,
    Close = 8,
    Claim = 9,
}

impl Kind {
    fn from_u8(v: u8) -> Result<Self, DecodeError> {
        use Kind::*;
        Ok(match v {
            0 => Genesis,
            1 => Keyholder,
            2 => Inviter,
            3 => Member,
            4 => Invite,
            5 => Revoke,
            6 => Retire,
            7 => Open,
            8 => Close,
            9 => Claim,
            k => return Err(DecodeError::Kind(k)),
        })
    }
    pub fn is_grant(self) -> bool {
        matches!(
            self,
            Kind::Keyholder | Kind::Inviter | Kind::Member | Kind::Invite
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    pub subject: Key,
    pub nonce: [u8; 16],
    pub issued_ms: u64,
    pub expiry_ms: u64,
    pub generation: u32,
    pub cites_revoke: RecordId,
    pub meta: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Revocation {
    pub subject: Key,
    pub nonce: [u8; 16],
    pub issued_ms: u64,
    pub cites_revoke: RecordId,
    /// Retire only: records issued by `subject` that stay valid. Strictly ascending.
    pub kept: Vec<RecordId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    Genesis {
        nonce: [u8; 16],
        created_ms: u64,
        meta: Vec<u8>,
    },
    Grant(Grant),
    Revocation(Revocation),
    Open {
        generation: u32,
        nonce: [u8; 16],
        issued_ms: u64,
    },
    Close {
        generation: u32,
        issued_ms: u64,
    },
    Claim {
        generation: u32,
        term: u32,
        rank: u64,
        issued_ms: u64,
    },
}

/// A decoded, signature-checked record. `bytes` is the exact wire encoding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub kind: Kind,
    pub room: RoomId,
    pub issuer: Key,
    pub body: Body,
    pub id: RecordId,
    pub bytes: Vec<u8>,
}

impl Record {
    pub fn grant(&self) -> Option<&Grant> {
        match &self.body {
            Body::Grant(g) => Some(g),
            _ => None,
        }
    }
    pub fn revocation(&self) -> Option<&Revocation> {
        match &self.body {
            Body::Revocation(r) => Some(r),
            _ => None,
        }
    }
    /// Subject of a grant or revocation.
    pub fn subject(&self) -> Option<Key> {
        match &self.body {
            Body::Grant(g) => Some(g.subject),
            Body::Revocation(r) => Some(r.subject),
            _ => None,
        }
    }
    pub fn cites_revoke(&self) -> RecordId {
        match &self.body {
            Body::Grant(g) => g.cites_revoke,
            Body::Revocation(r) => r.cites_revoke,
            _ => RecordId::ZERO,
        }
    }

    /// Build and sign a record. Panics on a body that would not decode (caller bug).
    pub fn sign(signer: &Identity, kind: Kind, room: RoomId, body: Body) -> Record {
        let mut w = Writer::new();
        w.raw(&RECORD_MAGIC).u8(RECORD_VERSION).u8(kind as u8);
        w.raw(&if kind == Kind::Genesis {
            [0; 32]
        } else {
            room.0
        });
        w.raw(&signer.key().0);
        encode_body(&mut w, kind, &body);
        let mut msg = RECORD_SIG_CTX.to_vec();
        msg.extend_from_slice(&w.0);
        let sig = signer.sign(&msg);
        w.raw(&sig);
        let bytes = w.finish();
        Record::decode(&bytes).expect("signed record must decode")
    }

    pub fn decode(bytes: &[u8]) -> Result<Record, DecodeError> {
        if bytes.len() < HEADER + 64 {
            return Err(DecodeError::Short);
        }
        let signed = &bytes[..bytes.len() - 64];
        let sig: [u8; 64] = bytes[bytes.len() - 64..].try_into().unwrap();
        let mut r = Reader::new(signed);
        if r.array::<2>()? != RECORD_MAGIC {
            return Err(DecodeError::Magic);
        }
        let v = r.u8()?;
        if v != RECORD_VERSION {
            return Err(DecodeError::Version(v));
        }
        let kind = Kind::from_u8(r.u8()?)?;
        let room_field = RoomId(r.array()?);
        let issuer = Key(r.array()?);
        let body = decode_body(&mut r, kind)?;
        r.end()?;
        let room = if kind == Kind::Genesis {
            if !room_field.is_zero() {
                return Err(DecodeError::Field("genesis room_id must be zero"));
            }
            let Body::Genesis {
                nonce,
                created_ms,
                meta,
            } = &body
            else {
                unreachable!()
            };
            room_id_of(issuer, nonce, *created_ms, meta)
        } else {
            if room_field.is_zero() {
                return Err(DecodeError::Field("room_id zero"));
            }
            room_field
        };
        let mut msg = RECORD_SIG_CTX.to_vec();
        msg.extend_from_slice(signed);
        if !issuer.verify(&msg, &sig) {
            return Err(DecodeError::Signature);
        }
        Ok(Record {
            kind,
            room,
            issuer,
            body,
            id: RecordId(h("prim/1 record-id", signed)),
            bytes: bytes.to_vec(),
        })
    }
}

pub fn room_id_of(root: Key, nonce: &[u8; 16], created_ms: u64, meta: &[u8]) -> RoomId {
    let mut w = Writer::new();
    w.raw(&root.0).raw(nonce).u64(created_ms).bytes16(meta);
    RoomId(h("prim/1 room-id", &w.0))
}

fn encode_body(w: &mut Writer, kind: Kind, body: &Body) {
    match (kind, body) {
        (
            Kind::Genesis,
            Body::Genesis {
                nonce,
                created_ms,
                meta,
            },
        ) => {
            w.raw(nonce).u64(*created_ms).bytes16(meta);
        }
        (k, Body::Grant(g)) if k.is_grant() => {
            w.raw(&g.subject.0)
                .raw(&g.nonce)
                .u64(g.issued_ms)
                .u64(g.expiry_ms)
                .u32(g.generation);
            w.raw(&g.cites_revoke.0).bytes16(&g.meta);
        }
        (Kind::Revoke | Kind::Retire, Body::Revocation(r)) => {
            w.raw(&r.subject.0)
                .raw(&r.nonce)
                .u64(r.issued_ms)
                .raw(&r.cites_revoke.0);
            if kind == Kind::Retire {
                w.u8(u8::try_from(r.kept.len()).expect("kept overflow"));
                for k in &r.kept {
                    w.raw(&k.0);
                }
            }
        }
        (
            Kind::Open,
            Body::Open {
                generation,
                nonce,
                issued_ms,
            },
        ) => {
            w.u32(*generation).raw(nonce).u64(*issued_ms);
        }
        (
            Kind::Close,
            Body::Close {
                generation,
                issued_ms,
            },
        ) => {
            w.u32(*generation).u64(*issued_ms);
        }
        (
            Kind::Claim,
            Body::Claim {
                generation,
                term,
                rank,
                issued_ms,
            },
        ) => {
            w.u32(*generation).u32(*term).u64(*rank).u64(*issued_ms);
        }
        _ => panic!("body does not match kind {kind:?}"),
    }
}

fn decode_body(r: &mut Reader<'_>, kind: Kind) -> Result<Body, DecodeError> {
    Ok(match kind {
        Kind::Genesis => {
            let nonce = r.array()?;
            let created_ms = r.u64()?;
            let meta = r.bytes16(META_CAP, "meta")?.to_vec();
            Body::Genesis {
                nonce,
                created_ms,
                meta,
            }
        }
        Kind::Keyholder | Kind::Inviter | Kind::Member | Kind::Invite => {
            let g = Grant {
                subject: Key(r.array()?),
                nonce: r.array()?,
                issued_ms: r.u64()?,
                expiry_ms: r.u64()?,
                generation: r.u32()?,
                cites_revoke: RecordId(r.array()?),
                meta: r.bytes16(META_CAP, "meta")?.to_vec(),
            };
            if (kind == Kind::Invite) != (g.generation != 0) {
                return Err(DecodeError::Field(
                    "generation is nonzero exactly for invites",
                ));
            }
            Body::Grant(g)
        }
        Kind::Revoke | Kind::Retire => {
            let mut rv = Revocation {
                subject: Key(r.array()?),
                nonce: r.array()?,
                issued_ms: r.u64()?,
                cites_revoke: RecordId(r.array()?),
                kept: Vec::new(),
            };
            if kind == Kind::Retire {
                let n = r.u8()? as usize;
                if n > KEPT_CAP {
                    return Err(DecodeError::TooLarge("kept"));
                }
                for _ in 0..n {
                    let id = RecordId(r.array()?);
                    if rv.kept.last().is_some_and(|last| *last >= id) {
                        return Err(DecodeError::Field("kept must be strictly ascending"));
                    }
                    rv.kept.push(id);
                }
            }
            Body::Revocation(rv)
        }
        Kind::Open => {
            let generation = r.u32()?;
            let nonce = r.array()?;
            let issued_ms = r.u64()?;
            if generation == 0 {
                return Err(DecodeError::Field("generation 0"));
            }
            Body::Open {
                generation,
                nonce,
                issued_ms,
            }
        }
        Kind::Close => {
            let generation = r.u32()?;
            let issued_ms = r.u64()?;
            if generation == 0 {
                return Err(DecodeError::Field("generation 0"));
            }
            Body::Close {
                generation,
                issued_ms,
            }
        }
        Kind::Claim => {
            let generation = r.u32()?;
            let term = r.u32()?;
            let rank = r.u64()?;
            let issued_ms = r.u64()?;
            if generation == 0 || term == 0 {
                return Err(DecodeError::Field("claim generation/term 0"));
            }
            Body::Claim {
                generation,
                term,
                rank,
                issued_ms,
            }
        }
    })
}

/// Convenience constructors used by the daemon, CLI and tests.
pub mod build {
    use super::*;

    pub fn genesis(root: &Identity, nonce: [u8; 16], created_ms: u64, meta: &[u8]) -> Record {
        Record::sign(
            root,
            Kind::Genesis,
            RoomId::ZERO,
            Body::Genesis {
                nonce,
                created_ms,
                meta: meta.to_vec(),
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn grant(
        issuer: &Identity,
        room: RoomId,
        kind: Kind,
        subject: Key,
        nonce: [u8; 16],
        issued_ms: u64,
        expiry_ms: u64,
        generation: u32,
        cites_revoke: RecordId,
    ) -> Record {
        assert!(kind.is_grant());
        Record::sign(
            issuer,
            kind,
            room,
            Body::Grant(Grant {
                subject,
                nonce,
                issued_ms,
                expiry_ms,
                generation,
                cites_revoke,
                meta: Vec::new(),
            }),
        )
    }

    pub fn revoke(
        issuer: &Identity,
        room: RoomId,
        subject: Key,
        nonce: [u8; 16],
        issued_ms: u64,
        cites: RecordId,
    ) -> Record {
        Record::sign(
            issuer,
            Kind::Revoke,
            room,
            Body::Revocation(Revocation {
                subject,
                nonce,
                issued_ms,
                cites_revoke: cites,
                kept: Vec::new(),
            }),
        )
    }

    pub fn retire(
        issuer: &Identity,
        room: RoomId,
        subject: Key,
        nonce: [u8; 16],
        issued_ms: u64,
        cites: RecordId,
        mut kept: Vec<RecordId>,
    ) -> Record {
        kept.sort();
        kept.dedup();
        Record::sign(
            issuer,
            Kind::Retire,
            room,
            Body::Revocation(Revocation {
                subject,
                nonce,
                issued_ms,
                cites_revoke: cites,
                kept,
            }),
        )
    }

    pub fn open(
        issuer: &Identity,
        room: RoomId,
        generation: u32,
        nonce: [u8; 16],
        issued_ms: u64,
    ) -> Record {
        Record::sign(
            issuer,
            Kind::Open,
            room,
            Body::Open {
                generation,
                nonce,
                issued_ms,
            },
        )
    }

    pub fn close(issuer: &Identity, room: RoomId, generation: u32, issued_ms: u64) -> Record {
        Record::sign(
            issuer,
            Kind::Close,
            room,
            Body::Close {
                generation,
                issued_ms,
            },
        )
    }

    pub fn claim(
        issuer: &Identity,
        room: RoomId,
        generation: u32,
        term: u32,
        rank: u64,
        issued_ms: u64,
    ) -> Record {
        Record::sign(
            issuer,
            Kind::Claim,
            room,
            Body::Claim {
                generation,
                term,
                rank,
                issued_ms,
            },
        )
    }
}
