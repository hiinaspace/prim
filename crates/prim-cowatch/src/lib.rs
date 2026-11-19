//! Shared playback for a prim room, as a sans-IO state machine.
//!
//! The host (a Godot extension, a standalone player controller, or the test simulator)
//! owns time, the room daemon connection and the media player. It feeds [`Event`]s with
//! the current time to [`Cowatch::handle`] and carries out the returned [`Action`]s.
//! Nothing here reads a clock, opens a socket or touches a player.
//!
//! Design and operating notes: `reports/prim-architecture-2026-11/tactical/p1-cowatch.md`
//! in S's vault; wire formats in [`wire`].

pub mod clock;
pub mod correct;
pub mod machine;
pub mod wire;

pub use machine::*;
