//! The room model and its commands. Moved to `prim_client::room` in round A1 so Godot,
//! `prim ctl` and the tests share it; this file keeps the window's old names.

pub use prim_client::room::{
    short, ChatLine, Cmd, Delivery, Handle, KeyEntry, MemberRow, Model, Presence, Route,
};
pub use prim_client::room::RoomOpts as AttachOpts;

/// Attach to the daemon at `socket`; fails if nothing answers there.
pub async fn attach(socket: &std::path::Path, opts: AttachOpts) -> std::io::Result<Handle> {
    drop(std::os::unix::net::UnixStream::connect(socket)?);
    Ok(Handle::attach(prim_client::transport::UnixTransport::new(socket), opts))
}
