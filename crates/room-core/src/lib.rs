//! Sans-IO core of the prim room daemon.
//!
//! Wire contracts: `docs/room/CONTRACTS.md`. Nothing here reads a clock, a socket or
//! the OS RNG: time is an input (`Now`), and randomness comes from a seeded `Entropy`.

pub mod api;
pub mod frame;
pub mod grants;
pub mod ids;
pub mod record;
pub mod room;
pub mod sim;
pub mod state;
pub mod staterec;
pub mod subs;
pub mod token;
pub mod wire;

pub use grants::{Epoch, GrantSet, Lifecycle};
pub use ids::{Entropy, Identity, Key, Now, RecordId, RoomId};
pub use record::{Kind, Record};
pub use room::{Persisted, Room};
