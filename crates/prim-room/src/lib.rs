//! The prim room daemon as a library: a tokio [`driver`] for `room_core::Room`,
//! [`transport`]s (iroh `prim/1`, in-memory), the [`state`] directory and the
//! provisional [`local`] socket API.

pub mod daemon;
pub mod driver;
pub mod local;
pub mod media_age;
pub mod rt;
pub mod state;
pub mod transport;
