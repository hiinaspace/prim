//! The room state machine. SKELETON: signatures are the contract with the daemon
//! (`crates/prim-room`); bodies are filled in by the room-core builder.

use crate::api::{ClientId, Config, Input, Output, Status, Update};
use crate::grants::GrantSet;
use crate::ids::{Key, Now, RoomId};
use crate::token::InviteToken;
use crate::wire::DecodeError;

/// What the daemon saves to disk and hands back to [`Room::resume`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Persisted {
    pub room: RoomId,
    /// Signed records (genesis first), wire bytes.
    pub records: Vec<Vec<u8>>,
    /// Durable persistent state records of every writer, wire bytes.
    pub state: Vec<Vec<u8>>,
}

impl Persisted {
    pub fn encode(&self) -> Vec<u8> {
        todo!()
    }
    pub fn decode(_bytes: &[u8]) -> Result<Persisted, DecodeError> {
        todo!()
    }
}

pub struct Room {
    _private: (),
}

impl Room {
    /// Found a new room: genesis (this identity is root) plus `open(1)`.
    pub fn found(_config: Config, _now: Now, _meta: &[u8]) -> Room {
        todo!()
    }
    /// Resume a room this node belongs to from persisted state. It stays as it was
    /// (open or closed); a closed room needs `LocalCommand::Reopen` by a keyholder.
    pub fn resume(_config: Config, _persisted: Persisted, _now: Now) -> Result<Room, DecodeError> {
        todo!()
    }
    /// Join with an invite token: dial the bootstrap peers and redeem.
    pub fn join(_config: Config, _token: InviteToken, _now: Now) -> Room {
        todo!()
    }
    pub fn handle(&mut self, _now: Now, _input: Input) {
        todo!()
    }
    pub fn poll_output(&mut self) -> Option<Output> {
        todo!()
    }
    /// Monotonic microseconds of the next deadline, if any.
    pub fn poll_timeout(&self) -> Option<u64> {
        todo!()
    }
    /// Coalesced updates for one subscription, at most `max`.
    pub fn drain(&mut self, _client: ClientId, _sub: u32, _max: usize) -> Vec<Update> {
        todo!()
    }
    pub fn persist(&self) -> Persisted {
        todo!()
    }
    pub fn status(&self, _now: Now) -> Status {
        todo!()
    }
    /// `None` while joining (before the genesis arrived).
    pub fn grants(&self) -> Option<&GrantSet> {
        todo!()
    }
    pub fn room_id(&self) -> RoomId {
        todo!()
    }
    pub fn key(&self) -> Key {
        todo!()
    }
}
