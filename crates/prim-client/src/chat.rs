//! Room chat and the small app-level state keys (member profile, clients) every client
//! writes (docs/room/WINDOW.md). Moved here from prim-ui in round A1.
//!
//! Nothing here is known to the daemon: chat is an ordinary app message channel, and
//! `member/<id>/profile` and `member/<id>/clients` are ordinary member-scope keys.

/// App message channel carrying room chat.
pub const CHAT_CHANNEL: u16 = 100;
/// First byte of every chat payload.
pub const CHAT_VERSION: u8 = 1;
/// Longest chat text in bytes (the daemon allows 64 KiB; a chat line never needs it).
pub const MAX_TEXT: usize = 2000;
/// Longest display name in bytes.
pub const MAX_NAME: usize = 32;

/// The name a member gets the first time it has none: `$PRIM_NAME` (also the old
/// `$PRIM_UI_NAME`), else the machine's host name.
pub fn default_name() -> String {
    for var in ["PRIM_NAME", "PRIM_UI_NAME"] {
        if let Ok(n) = std::env::var(var) {
            if !n.trim().is_empty() {
                return clip(n.trim(), MAX_NAME).to_string();
            }
        }
    }
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "me".into());
    clip(&host, MAX_NAME).to_string()
}

/// `[version u8 = 1] [UTF-8 text]`. Over-long text is cut at a character boundary.
pub fn encode_chat(text: &str) -> Vec<u8> {
    let t = clip(text.trim(), MAX_TEXT);
    let mut v = Vec::with_capacity(1 + t.len());
    v.push(CHAT_VERSION);
    v.extend_from_slice(t.as_bytes());
    v
}

/// `None` for an unknown version, bad UTF-8 or empty text (the message is dropped).
pub fn decode_chat(b: &[u8]) -> Option<String> {
    let (&ver, rest) = b.split_first()?;
    if ver != CHAT_VERSION || rest.len() > MAX_TEXT {
        return None;
    }
    let s = std::str::from_utf8(rest).ok()?.trim();
    (!s.is_empty()).then(|| s.to_string())
}

pub fn clip(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut n = max;
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    &s[..n]
}

/// `member/<id>/profile`: `{"v":1,"name":"..."}`, persistent.
pub fn encode_profile(name: &str) -> Vec<u8> {
    serde_json::json!({"v": 1, "name": clip(name.trim(), MAX_NAME)})
        .to_string()
        .into_bytes()
}

pub fn decode_profile(b: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(b).ok()?;
    let n = v.get("name")?.as_str()?.trim();
    (!n.is_empty()).then(|| clip(n, MAX_NAME).to_string())
}

/// `member/<id>/clients`: which of this member's local programs are running, beyond
/// what the daemon itself knows (the voice lease is in `member/<id>/voice`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Clients {
    pub godot: bool,
    pub player: bool,
    /// Voice is running but the output is silenced (the voice key has no such bit).
    pub deaf: bool,
}

pub fn encode_clients(c: Clients) -> Vec<u8> {
    serde_json::json!({"v": 1, "godot": c.godot, "player": c.player, "deaf": c.deaf})
        .to_string()
        .into_bytes()
}

pub fn decode_clients(b: &[u8]) -> Option<Clients> {
    let v: serde_json::Value = serde_json::from_slice(b).ok()?;
    let f = |k| v.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
    Some(Clients {
        godot: f("godot"),
        player: f("player"),
        deaf: f("deaf"),
    })
}

/// `member/<id>/voice` (CONTRACTS.md section 6): `(listening, mic)`.
pub fn decode_voice(b: &[u8]) -> Option<(bool, bool)> {
    if b.len() < 2 || b[0] != 1 {
        return None;
    }
    Some((b[1] & 1 != 0, b[1] & 2 != 0))
}

/// Local wall-clock time as `HH:MM` (or `HH:MM:SS`).
pub fn clock(t: std::time::SystemTime, secs: bool) -> String {
    let s = t
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as libc::time_t)
        .unwrap_or(0);
    // SAFETY: localtime_r writes into our zeroed tm.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&s, &mut tm);
        tm
    };
    if secs {
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    } else {
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_round_trips_and_rejects() {
        assert_eq!(decode_chat(&encode_chat("  hi é ")).as_deref(), Some("hi é"));
        assert_eq!(decode_chat(&[2, b'x']), None, "unknown version");
        assert_eq!(decode_chat(&[1, 0xff]), None, "bad utf-8");
        assert_eq!(decode_chat(&[1]), None, "empty");
        let long = "é".repeat(3000);
        let enc = encode_chat(&long);
        assert!(enc.len() <= 1 + MAX_TEXT && decode_chat(&enc).is_some());
    }

    #[test]
    fn profile_and_clients_round_trip() {
        assert_eq!(decode_profile(&encode_profile("S")).as_deref(), Some("S"));
        assert_eq!(decode_profile(b"{}"), None);
        let c = Clients {
            godot: true,
            player: false,
            deaf: true,
        };
        assert_eq!(decode_clients(&encode_clients(c)), Some(c));
        assert_eq!(decode_voice(&[1, 3, 0, 0, 0, 0]), Some((true, true)));
        assert_eq!(decode_voice(&[1, 1, 0, 0, 0, 0]), Some((true, false)));
    }
}
