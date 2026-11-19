//! Client for the prim room daemon's local API (`room.sock`).
//!
//! Layers: [`transport`] (frozen trait: how bytes get to the daemon), [`wire`] (JSON
//! message types, mirroring `prim_room::local`), [`client`] (worker thread, reconnect,
//! request/reply). Godot wraps [`client::Client`] as `PrimClient` in `native/`.
//!
//! The wire types are mirrored rather than imported so this crate builds without the
//! daemon (and later on Windows); `tests/daemon.rs` runs it against the real daemon, so
//! drift fails a test.

pub mod client;
pub mod transport;
pub mod wire;

pub use client::{Client, ClientEvent};
pub use transport::{Conn, Transport};
