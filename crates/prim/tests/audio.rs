//! Round GW2 oracle: the audio settings, meter and loopback test, run as the real
//! `prim voice` subcommands against private tone sources and a wav sink. No audio device
//! is opened: never S's microphone or speakers.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn prim() -> Command {
    Command::new(env!("CARGO_BIN_EXE_prim"))
}

fn settings(dir: &Path, gain_db: f32) -> PathBuf {
    let p = dir.join("audio.json");
    std::fs::write(&p, format!(r#"{{"v":1,"input":null,"output":null,"gain_db":{gain_db},"volume":1.0}}"#)).unwrap();
    p
}

fn levels(p: &Path) -> Option<prim_ui::audio::Reading> {
    prim_ui::audio::read_levels(p)
}

fn settle_peak(p: &Path, want: f32, within: Duration) -> Result<f32, f32> {
    let t = Instant::now();
    let mut last = 0.0;
    while t.elapsed() < within {
        if let Some(r) = levels(p) {
            last = r.peak;
            if (r.peak - want).abs() < 0.02 {
                return Ok(r.peak);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(last)
}

/// The meter shows what is sent: gain is applied before it, the saved gain is picked up
/// when the process starts, and a change in the settings file moves it while it runs.
#[test]
fn meter_reads_saved_gain_and_follows_changes() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = settings(dir.path(), 6.0);
    let lv = dir.path().join("meter.levels");
    let mut child = prim()
        .args(["voice", "meter", "--capture=tone:440", "--duration=20"])
        .arg(format!("--levels={}", lv.display()))
        .env("PRIM_AUDIO_SETTINGS", &cfg)
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    // tone amplitude 0.3, +6 dB is x1.995
    let r = settle_peak(&lv, 0.3 * 1.9953, Duration::from_secs(5));
    assert!(r.is_ok(), "saved +6 dB gain not applied on start: peak {r:?}");
    settings(dir.path(), 0.0);
    let r = settle_peak(&lv, 0.3, Duration::from_secs(5));
    assert!(r.is_ok(), "gain change in the settings file did not reach the running meter: peak {r:?}");
    let seq = levels(&lv).unwrap().seq;
    std::thread::sleep(Duration::from_millis(300));
    assert!(levels(&lv).unwrap().seq > seq + 5, "levels are published about 30 times a second");
    let _ = child.kill();
    let _ = child.wait();
}

/// Clipping is counted after gain: +20 dB on a 0.3 tone is far past full scale.
#[test]
fn meter_counts_clipping() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = settings(dir.path(), 20.0);
    let lv = dir.path().join("meter.levels");
    let mut child = prim()
        .args(["voice", "meter", "--capture=tone:440", "--duration=20"])
        .arg(format!("--levels={}", lv.display()))
        .env("PRIM_AUDIO_SETTINGS", &cfg)
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(5) && levels(&lv).is_none_or(|r| r.clips == 0) {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(levels(&lv).is_some_and(|r| r.clips > 0), "clipping not counted");
    let _ = child.kill();
    let _ = child.wait();
}

fn read_wav_mono(p: &Path) -> Vec<f32> {
    let mut r = hound::WavReader::open(p).unwrap();
    let s: Vec<f32> = r.samples::<f32>().map(|s| s.unwrap()).collect();
    s.chunks(2).map(|c| c[0]).collect()
}

/// The loopback goes through the real codec: after gain, a 3 s tone comes back at the
/// same pitch and level, as a few kB of Opus, and is not the source sample for sample.
#[test]
fn loopback_goes_through_opus() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = settings(dir.path(), 6.0);
    let out = dir.path().join("out.wav");
    let o = prim()
        .args(["voice", "test", "--capture=tone:440", "--seconds=3"])
        .arg(format!("--output=wav:{}", out.display()))
        .env("PRIM_AUDIO_SETTINGS", &cfg)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let text = String::from_utf8_lossy(&o.stdout);
    let phases: Vec<serde_json::Value> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let ph = |n: &str| phases.iter().any(|v| v["phase"] == n);
    assert!(ph("recording") && ph("playing") && ph("done"), "phases: {text}");
    let done = phases.iter().find(|v| v["phase"] == "done").unwrap();
    let bytes = done["bytes"].as_u64().unwrap();
    // 3 s at 16 kbps is ~6 kB: Opus, not 288 kB of float PCM
    assert!((3_000..=12_000).contains(&bytes), "sent {bytes} bytes");

    let m = read_wav_mono(&out);
    let sr = 48_000.0f64;
    // the steady middle of the tone, away from the start-up and the tail
    let a = (0.6 * sr) as usize;
    let seg = &m[a..a + (1.5 * sr) as usize];
    let peak = seg.iter().fold(0f32, |p, s| p.max(s.abs()));
    assert!((peak - 0.6).abs() < 0.08, "level after +6 dB gain: peak {peak}");
    // frequency by zero crossings
    let zc = seg.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count() as f64;
    let hz = zc / (seg.len() as f64 / sr);
    assert!((hz - 440.0).abs() < 8.0, "pitch {hz} Hz");
    // not a bit-exact copy of an ideal tone: best-fit sinusoid leaves a codec residue
    let (mut sc, mut ss, mut cc, mut sn) = (0f64, 0f64, 0f64, 0f64);
    for (i, s) in seg.iter().enumerate() {
        let t = 2.0 * std::f64::consts::PI * 440.0 * i as f64 / sr;
        sc += *s as f64 * t.cos();
        ss += *s as f64 * t.sin();
        cc += t.cos().powi(2);
        sn += t.sin().powi(2);
    }
    let (ac, as_) = (sc / cc, ss / sn);
    let mut res = 0f64;
    let mut tot = 0f64;
    for (i, s) in seg.iter().enumerate() {
        let t = 2.0 * std::f64::consts::PI * 440.0 * i as f64 / sr;
        let fit = ac * t.cos() + as_ * t.sin();
        res += (*s as f64 - fit).powi(2);
        tot += (*s as f64).powi(2);
    }
    let rel = (res / tot).sqrt();
    assert!(rel > 2e-3, "output is too clean ({rel:.5}): the codec was bypassed");
    assert!(rel < 0.3, "output is mostly not the tone ({rel:.3})");
}
