//! Invite token (CONTRACTS.md section 7).

use crate::ids::{Identity, Key, RecordId, RoomId};
use crate::record::{build, Kind, Record};
use crate::wire::{h, DecodeError, Reader, Writer};

pub const TOKEN_PREFIX: &str = "prim1";
pub const TOKEN_VERSION: u8 = 1;
pub const PEER_CAP: usize = 4;
pub const RELAY_CAP: usize = 2;
pub const RELAY_LEN_CAP: usize = 100;

#[derive(Clone, Debug)]
pub struct InviteToken {
    pub room: RoomId,
    pub invite: Record,
    pub ephemeral: Identity,
    pub peers: Vec<Key>,
    pub relays: Vec<String>,
}

impl PartialEq for InviteToken {
    fn eq(&self, o: &Self) -> bool {
        self.room == o.room
            && self.invite == o.invite
            && self.ephemeral.secret() == o.ephemeral.secret()
            && self.peers == o.peers
            && self.relays == o.relays
    }
}

const B32: data_encoding::Encoding = data_encoding::BASE32_NOPAD;

impl InviteToken {
    pub fn encode(&self) -> String {
        let mut w = Writer::new();
        w.u8(TOKEN_VERSION)
            .raw(&self.room.0)
            .bytes16(&self.invite.bytes)
            .raw(&self.ephemeral.secret());
        assert!(self.peers.len() <= PEER_CAP && self.relays.len() <= RELAY_CAP);
        w.u8(self.peers.len() as u8);
        for p in &self.peers {
            w.raw(&p.0);
        }
        w.u8(self.relays.len() as u8);
        for r in &self.relays {
            assert!(r.len() <= RELAY_LEN_CAP);
            w.bytes8(r.as_bytes());
        }
        let check = h("prim/1 token-check", &w.0);
        w.raw(&check[..4]);
        format!("{TOKEN_PREFIX}{}", B32.encode(&w.0).to_ascii_lowercase())
    }

    pub fn decode(s: &str) -> Result<InviteToken, DecodeError> {
        let s = s.trim();
        let body = s.strip_prefix(TOKEN_PREFIX).ok_or(DecodeError::Magic)?;
        let raw = B32
            .decode(body.to_ascii_uppercase().as_bytes())
            .map_err(|_| DecodeError::Field("base32"))?;
        if raw.len() < 5 {
            return Err(DecodeError::Short);
        }
        let (payload, check) = raw.split_at(raw.len() - 4);
        if h("prim/1 token-check", payload)[..4] != *check {
            return Err(DecodeError::Field("checksum"));
        }
        let mut r = Reader::new(payload);
        let v = r.u8()?;
        if v != TOKEN_VERSION {
            return Err(DecodeError::Version(v));
        }
        let room = RoomId(r.array()?);
        let invite = Record::decode(r.bytes16(4096, "invite")?)?;
        let ephemeral = Identity::from_secret(r.array()?);
        let n = r.u8()? as usize;
        if n > PEER_CAP {
            return Err(DecodeError::TooLarge("peers"));
        }
        let mut peers = Vec::new();
        for _ in 0..n {
            peers.push(Key(r.array()?));
        }
        let m = r.u8()? as usize;
        if m > RELAY_CAP {
            return Err(DecodeError::TooLarge("relays"));
        }
        let mut relays = Vec::new();
        for _ in 0..m {
            let b = r.bytes8(RELAY_LEN_CAP, "relay")?;
            relays
                .push(String::from_utf8(b.to_vec()).map_err(|_| DecodeError::Field("relay utf8"))?);
        }
        r.end()?;
        if invite.kind != Kind::Invite || invite.room != room {
            return Err(DecodeError::Field("not an invite for this room"));
        }
        if invite.grant().unwrap().subject != ephemeral.key() {
            return Err(DecodeError::Field("ephemeral key does not match invite"));
        }
        Ok(InviteToken {
            room,
            invite,
            ephemeral,
            peers,
            relays,
        })
    }

    /// Mint: the inviter signs an invite grant for a fresh ephemeral key.
    #[allow(clippy::too_many_arguments)]
    pub fn mint(
        inviter: &Identity,
        room: RoomId,
        generation: u32,
        ephemeral_secret: [u8; 32],
        nonce: [u8; 16],
        issued_ms: u64,
        expiry_ms: u64,
        peers: Vec<Key>,
        relays: Vec<String>,
    ) -> InviteToken {
        let ephemeral = Identity::from_secret(ephemeral_secret);
        let invite = build::grant(
            inviter,
            room,
            Kind::Invite,
            ephemeral.key(),
            nonce,
            issued_ms,
            expiry_ms,
            generation,
            RecordId::ZERO,
        );
        InviteToken {
            room,
            invite,
            ephemeral,
            peers,
            relays,
        }
    }

    /// Redeem: the joiner's member grant, signed with the ephemeral key. Returns the
    /// hello proof `[invite, member]`.
    pub fn redeem(&self, joiner: Key, nonce: [u8; 16], issued_ms: u64) -> [Record; 2] {
        let member = build::grant(
            &self.ephemeral,
            self.room,
            Kind::Member,
            joiner,
            nonce,
            issued_ms,
            0,
            0,
            RecordId::ZERO,
        );
        [self.invite.clone(), member]
    }
}
