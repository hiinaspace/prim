//! A tiny JSONL event sink shared by every Prim component and the loop scripts.
//!
//! Each event is one line `{"t_mono_us":..,"t_unix_ms":..,"src":"..","ev":"..", ...fields}`
//! appended to the file named by `PRIM_EVENT_LOG` (or given to [`Sink::open`]). Lines are
//! written with one `write(2)` on an `O_APPEND` descriptor, so several processes can
//! share one file without interleaving inside a line.
//!
//! `t_mono_us` is `CLOCK_MONOTONIC` in microseconds: comparable across processes on one
//! host, and equal to Python's `time.monotonic_ns() // 1000`. Godot's own
//! `Time.get_ticks_usec` is offset from process start, so Godot code calls through
//! `PrimEvents` (native/) instead.
//!
//! Standard event names ([`ev`]): `spawn`, `daemon_ready`, `joined`, `first_frame`,
//! `first_voice`. Hook points:
//! - `first_voice`: `crates/prim-voice`, where the first decoded frame reaches the sink.
//!   (Not edited this round; another session owns that crate.)
//! - `daemon_ready`: `crates/prim` `room` subcommand, after the local socket is bound.
//!   The loop script derives it from socket appearance until then.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use serde_json::{Map, Value};

pub mod ev {
    pub const SPAWN: &str = "spawn";
    pub const DAEMON_READY: &str = "daemon_ready";
    pub const JOINED: &str = "joined";
    pub const FIRST_FRAME: &str = "first_frame";
    pub const FIRST_VOICE: &str = "first_voice";
}

/// Env var naming the shared log file.
pub const ENV_LOG: &str = "PRIM_EVENT_LOG";

/// `CLOCK_MONOTONIC` in microseconds.
pub fn mono_us() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: ts is a valid out pointer.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000 + ts.tv_nsec as u64 / 1_000
}

pub fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Format one event line (no trailing newline). `fields` must be a JSON object or null.
pub fn format_line(src: &str, event: &str, fields: &Value, t_mono_us: u64, t_unix_ms: u64) -> String {
    let mut m = Map::new();
    m.insert("t_mono_us".into(), t_mono_us.into());
    m.insert("t_unix_ms".into(), t_unix_ms.into());
    m.insert("src".into(), src.into());
    m.insert("ev".into(), event.into());
    if let Value::Object(f) = fields {
        for (k, v) in f {
            // The four standard keys are not overridable.
            m.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
    Value::Object(m).to_string()
}

/// An append-only event file. A sink with no file discards events.
pub struct Sink {
    src: String,
    file: Mutex<Option<File>>,
}

impl Sink {
    /// A sink that discards everything.
    pub fn null(src: &str) -> Sink {
        Sink {
            src: src.into(),
            file: Mutex::new(None),
        }
    }

    /// Append to `path` (created if absent).
    pub fn open(src: &str, path: &Path) -> std::io::Result<Sink> {
        let f = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Sink {
            src: src.into(),
            file: Mutex::new(Some(f)),
        })
    }

    /// Open `$PRIM_EVENT_LOG`; a null sink when it is unset or unwritable.
    pub fn from_env(src: &str) -> Sink {
        match std::env::var_os(ENV_LOG).filter(|v| !v.is_empty()) {
            Some(p) => Sink::open(src, Path::new(&p)).unwrap_or_else(|_| Sink::null(src)),
            None => Sink::null(src),
        }
    }

    pub fn is_null(&self) -> bool {
        self.file.lock().unwrap().is_none()
    }

    /// Record `event` with extra object `fields`, stamped now.
    pub fn emit(&self, event: &str, fields: Value) {
        let mut g = self.file.lock().unwrap();
        if let Some(f) = g.as_mut() {
            let mut line = format_line(&self.src, event, &fields, mono_us(), unix_ms());
            line.push('\n');
            let _ = f.write_all(line.as_bytes());
        }
    }

    /// Record `event` only the first time this sink sees it (per `Once` flag).
    pub fn emit_once(&self, once: &std::sync::Once, event: &str, fields: Value) {
        once.call_once(|| self.emit(event, fields));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lines_are_json_with_standard_keys_that_cannot_be_overridden() {
        let l = format_line("bot", "joined", &json!({"member":"ab","ev":"x","t_mono_us":1}), 7, 9);
        let v: Value = serde_json::from_str(&l).unwrap();
        assert_eq!(v["ev"], "joined");
        assert_eq!(v["t_mono_us"], 7);
        assert_eq!(v["member"], "ab");
        assert_eq!(v["src"], "bot");
    }

    #[test]
    fn two_sinks_share_a_file_and_stay_monotonic() {
        let d = std::env::temp_dir().join(format!("prim-events-test-{}", std::process::id()));
        let _ = std::fs::remove_file(&d);
        let a = Sink::open("a", &d).unwrap();
        let b = Sink::open("b", &d).unwrap();
        a.emit(ev::SPAWN, json!({}));
        b.emit(ev::JOINED, json!({"n": 2}));
        a.emit(ev::FIRST_FRAME, Value::Null);
        let text = std::fs::read_to_string(&d).unwrap();
        let lines: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 3);
        let t: Vec<u64> = lines.iter().map(|l| l["t_mono_us"].as_u64().unwrap()).collect();
        assert!(t.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(lines[1]["n"], 2);
        std::fs::remove_file(&d).unwrap();
    }

    #[test]
    fn null_sink_discards() {
        let s = Sink::null("x");
        assert!(s.is_null());
        s.emit("spawn", Value::Null);
    }
}
