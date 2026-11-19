//! Where Prim's local sockets and logs live, and how to find the `prim` binary. Shared by
//! the session supervisor, `prim ctl`, the dev window and Godot.

use std::path::{Path, PathBuf};

/// The room daemon's local API socket: `$PRIM_SOCKET`, else
/// `$XDG_RUNTIME_DIR/prim/room.sock` (the same default `prim room` uses).
pub fn room_socket() -> PathBuf {
    if let Some(p) = std::env::var_os("PRIM_SOCKET").filter(|v| !v.is_empty()) {
        return PathBuf::from(p);
    }
    match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d).join("prim").join("room.sock"),
        None => PathBuf::from(format!("/tmp/prim-{}/room.sock", uid())),
    }
}

fn uid() -> String {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self")
        .map(|m| m.uid().to_string())
        .unwrap_or_else(|_| "0".into())
}

pub fn rt_socket(room_sock: &Path) -> PathBuf {
    room_sock.with_file_name("rt.sock")
}

/// The session supervisor's control socket, beside `room.sock`.
pub fn session_socket(room_sock: &Path) -> PathBuf {
    room_sock.with_file_name("session.sock")
}

/// Where `prim voice` publishes the live microphone level while it is running.
pub fn voice_levels(room_sock: &Path) -> PathBuf {
    room_sock.with_file_name("voice.levels")
}

/// Logs of what the session spawned (`room.log`, `voice.log`, `session.log`).
pub fn log_dir() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."));
    let d = base.join("prim").join("session");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// The `prim` binary to spawn: `$PRIM_BIN` (also the old `$PRIM_UI_EXE`), then a sibling
/// of this executable, then `prim` on `PATH`.
pub fn prim_exe() -> PathBuf {
    for var in ["PRIM_BIN", "PRIM_UI_EXE"] {
        if let Some(p) = std::env::var_os(var).filter(|v| !v.is_empty()) {
            return PathBuf::from(p);
        }
    }
    if let Ok(me) = std::env::current_exe() {
        if me.file_name().is_some_and(|n| n == "prim") {
            return me;
        }
        if let Some(sib) = me.parent().map(|d| d.join("prim")).filter(|p| p.exists()) {
            return sib;
        }
    }
    PathBuf::from("prim")
}
