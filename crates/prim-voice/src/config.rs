//! Command-line surface: `prim voice [options]` and `prim voice track [options]`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};

use crate::seats::SeatSel;

/// What `prim` hands us after it has scanned the command line (`crate::Args` in the
/// binary): this crate has no access to that type, so it asks through a trait.
pub trait Flags {
    fn positional(&self) -> &[String];
    fn has(&self, name: &str) -> bool;
    fn value(&self, name: &str) -> Option<&str>;
    fn values(&self, name: &str) -> Vec<&str>;
}

/// The flags that take a value (the rest are booleans), for the caller's scanner.
pub const VALUED_FLAGS: &[&str] = &[
    "rt-socket",
    "capture",
    "output",
    "skew-ppm",
    "seat",
    "seat-index",
    "stats",
    "rate",
    "sweep",
    "duration",
    "incarnation",
    "arrival",
    "min-delay-ms",
];

pub const HELP: &str = "\
prim voice [options]            the voice client (one per daemon)
prim voice track [options]      send head poses as the tracking source

options:
  --rt-socket PATH      realtime socket (default $XDG_RUNTIME_DIR/prim/rt.sock)
  --takeover            take the lease from a running client instead of failing
  --capture SRC         device (default input) | wav:PATH (looped) | tone:HZ | tone:auto | none
  --skew-ppm N          pace a wav/tone capture N ppm fast (negative: slow)
  --output SINK         device (default output) | wav:PATH | null
  --seat KEYHEX=DEG     put a member at an azimuth (repeatable; a key prefix of 4+ hex digits works)
  --seat-index N=DEG    same, by index among the other members sorted by key
  --stats PATH          write JSON stats every second and at exit
  --stats-tone          per output second, the energy at each tone:auto frequency seen (in --stats)
  --min-delay-ms N      NetEq minimum delay (default 20, voice-core's)
  --no-dtx              send silence instead of using Opus DTX
  --duration SECS       exit after this long (default: run until SIGINT/SIGTERM)
  SIGUSR1               reopen the audio devices on the current default

track options:
  --rate HZ             pose rate (default 90)
  --sweep DEG_PER_S     yaw sweep speed (default 30)
";

#[derive(Clone, Debug)]
pub enum CaptureSpec {
    None,
    Device,
    Wav(PathBuf),
    Tone(f32),
    /// A frequency derived from the incarnation (`tone::auto_hz`).
    ToneAuto,
}

/// A shared sample buffer for in-process tests and probes.
pub type OutputBuffer = Arc<Mutex<Vec<f32>>>;

#[derive(Clone)]
pub enum OutputSpec {
    Null,
    Device,
    Wav(PathBuf),
    /// Interleaved stereo f32 appended here, paced like the other sinks.
    Capture(OutputBuffer),
}

/// How a packet's arrival time is given to NetEq.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrivalPolicy {
    /// The daemon's `received_at_mono_us` stamp (R0 design: NetEq sees network jitter only).
    Stamp,
    /// The time this process read the message; for measuring what the stamp buys.
    ReadTime,
}

#[derive(Clone)]
pub struct Config {
    pub takeover: bool,
    pub capture: CaptureSpec,
    pub skew_ppm: f64,
    pub output: OutputSpec,
    pub seats: Vec<(SeatSel, f32)>,
    pub stats_path: Option<PathBuf>,
    pub stats_tone: bool,
    pub dtx: bool,
    pub arrival: ArrivalPolicy,
    /// NetEq minimum delay (None: voice-core's default of 20 ms).
    pub min_delay_ms: Option<u32>,
    /// Tests: fix the incarnation instead of drawing one.
    pub incarnation: Option<u32>,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            takeover: false,
            capture: CaptureSpec::None,
            skew_ppm: 0.0,
            output: OutputSpec::Null,
            seats: vec![],
            stats_path: None,
            stats_tone: false,
            dtx: true,
            arrival: ArrivalPolicy::Stamp,
            min_delay_ms: None,
            incarnation: None,
        }
    }
}

fn parse_capture(s: &str) -> Result<CaptureSpec> {
    Ok(match s {
        "none" => CaptureSpec::None,
        "device" => CaptureSpec::Device,
        "tone:auto" => CaptureSpec::ToneAuto,
        _ => {
            if let Some(p) = s.strip_prefix("wav:") {
                CaptureSpec::Wav(p.into())
            } else if let Some(h) = s.strip_prefix("tone:") {
                CaptureSpec::Tone(h.parse().with_context(|| format!("tone frequency {h:?}"))?)
            } else {
                bail!("--capture {s:?}: expected device, wav:PATH, tone:HZ, tone:auto or none")
            }
        }
    })
}

fn parse_output(s: &str) -> Result<OutputSpec> {
    Ok(match s {
        "null" => OutputSpec::Null,
        "device" => OutputSpec::Device,
        _ => match s.strip_prefix("wav:") {
            Some(p) => OutputSpec::Wav(p.into()),
            None => bail!("--output {s:?}: expected device, wav:PATH or null"),
        },
    })
}

fn parse_seat(s: &str, by_index: bool) -> Result<(SeatSel, f32)> {
    let (k, az) = s
        .split_once('=')
        .with_context(|| format!("seat {s:?}: expected NAME=DEGREES"))?;
    let az: f32 = az.parse().with_context(|| format!("azimuth {az:?}"))?;
    let sel = if by_index {
        SeatSel::Index(k.parse().with_context(|| format!("seat index {k:?}"))?)
    } else {
        if k.len() < 4 || !k.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("seat key {k:?}: expected at least 4 hex digits");
        }
        SeatSel::KeyPrefix(k.to_ascii_lowercase())
    };
    Ok((sel, az))
}

impl Config {
    pub fn from_flags(f: &dyn Flags) -> Result<Config> {
        let mut c = Config::default();
        let device = cfg!(feature = "device");
        c.capture =
            parse_capture(
                f.value("capture")
                    .unwrap_or(if device { "device" } else { "none" }),
            )?;
        c.output =
            parse_output(
                f.value("output")
                    .unwrap_or(if device { "device" } else { "null" }),
            )?;
        if !device
            && (matches!(c.capture, CaptureSpec::Device) || matches!(c.output, OutputSpec::Device))
        {
            bail!("this build has no audio devices (feature `device` is off)");
        }
        c.takeover = f.has("takeover");
        if let Some(v) = f.value("skew-ppm") {
            c.skew_ppm = v.parse().with_context(|| format!("--skew-ppm {v:?}"))?;
        }
        for s in f.values("seat") {
            c.seats.push(parse_seat(s, false)?);
        }
        for s in f.values("seat-index") {
            c.seats.push(parse_seat(s, true)?);
        }
        c.stats_path = f.value("stats").map(PathBuf::from);
        c.stats_tone = f.has("stats-tone");
        c.dtx = !f.has("no-dtx");
        if let Some(v) = f.value("incarnation") {
            let n: u32 = v.parse().with_context(|| format!("--incarnation {v:?}"))?;
            if n == 0 {
                bail!("--incarnation must be nonzero");
            }
            c.incarnation = Some(n);
        }
        if let Some(v) = f.value("min-delay-ms") {
            c.min_delay_ms = Some(v.parse().with_context(|| format!("--min-delay-ms {v:?}"))?);
        }
        c.arrival = match f.value("arrival") {
            None | Some("stamp") => ArrivalPolicy::Stamp,
            Some("read") => ArrivalPolicy::ReadTime,
            Some(o) => bail!("--arrival {o:?}: expected stamp or read"),
        };
        Ok(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_parse() {
        assert!(matches!(parse_capture("tone:440").unwrap(), CaptureSpec::Tone(f) if f == 440.0));
        assert!(matches!(
            parse_capture("tone:auto").unwrap(),
            CaptureSpec::ToneAuto
        ));
        assert!(parse_capture("tone:x").is_err());
        assert!(matches!(
            parse_output("wav:/tmp/x.wav").unwrap(),
            OutputSpec::Wav(_)
        ));
        let (s, az) = parse_seat("0A0b=-45", false).unwrap();
        assert_eq!(s, SeatSel::KeyPrefix("0a0b".into()));
        assert_eq!(az, -45.0);
        assert_eq!(parse_seat("2=90", true).unwrap().0, SeatSel::Index(2));
        assert!(parse_seat("zz=1", false).is_err());
    }
}
