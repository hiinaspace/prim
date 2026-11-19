//! Per-computer audio settings for the voice client and its window: which microphone and
//! speakers to use (by name, overriding the OS default), input gain, output volume, a live
//! level meter, and the microphone loopback test.
//!
//! Nothing here touches the wire. The settings are a small JSON file on this computer
//! (`$XDG_CONFIG_HOME/prim/audio.json`, or `PRIM_AUDIO_SETTINGS`), read by `prim voice` and
//! written by the room window. Levels go to a small file the window polls (`--levels PATH`,
//! 32 bytes rewritten ~30 times a second), so there is no extra socket and the room's
//! `rt.sock` stays voice frames plus listener pose.
//!
//! Where processing would slot in later: `Audio::condition_input` is the one place the
//! capture path touches samples before the encoder (and before the meter). Noise
//! suppression, AGC or echo cancellation belong there, after gain and before the meter
//! reads, so the meter and the loopback test show what others get.

use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::config::Flags;
use crate::ring::Ring;

/// A device as saved and as listed: a backend id (stable) and a display name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceRef {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// None: the system default.
    pub input: Option<DeviceRef>,
    pub output: Option<DeviceRef>,
    pub gain_db: f32,
    /// Linear output volume, 0.0..=2.0.
    pub volume: f32,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            input: None,
            output: None,
            gain_db: 0.0,
            volume: 1.0,
        }
    }
}

pub const GAIN_DB_RANGE: (f32, f32) = (-20.0, 30.0);

fn dev_to_json(d: &Option<DeviceRef>) -> Value {
    match d {
        Some(d) => json!({"id": d.id, "name": d.name}),
        None => Value::Null,
    }
}

fn dev_from_json(v: &Value) -> Option<DeviceRef> {
    Some(DeviceRef {
        id: v.get("id")?.as_str()?.to_string(),
        name: v.get("name")?.as_str()?.to_string(),
    })
}

impl Settings {
    pub fn path() -> PathBuf {
        if let Some(p) = std::env::var_os("PRIM_AUDIO_SETTINGS").filter(|v| !v.is_empty()) {
            return PathBuf::from(p);
        }
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("prim").join("audio.json")
    }

    /// Missing or unreadable file: the defaults (nothing saved is not an error).
    pub fn load() -> Settings {
        Settings::load_from(&Settings::path())
    }

    pub fn load_from(path: &Path) -> Settings {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Settings::default();
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else {
            return Settings::default();
        };
        Settings {
            input: v.get("input").and_then(dev_from_json),
            output: v.get("output").and_then(dev_from_json),
            gain_db: v
                .get("gain_db")
                .and_then(Value::as_f64)
                .map(|g| (g as f32).clamp(GAIN_DB_RANGE.0, GAIN_DB_RANGE.1))
                .unwrap_or(0.0),
            volume: v
                .get("volume")
                .and_then(Value::as_f64)
                .map(|g| (g as f32).clamp(0.0, 2.0))
                .unwrap_or(1.0),
        }
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&Settings::path())
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let v = json!({"v": 1, "input": dev_to_json(&self.input), "output": dev_to_json(&self.output),
                       "gain_db": self.gain_db, "volume": self.volume});
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&v)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

pub fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// What a saved device resolves to against the devices that exist now.
#[derive(Debug, PartialEq, Eq)]
pub enum Resolved {
    /// Nothing was chosen: the system default.
    Default,
    /// Index into the list.
    Named(usize),
    /// The saved device is gone: use the system default and say so.
    Missing(String),
}

/// Match by backend id first, then by display name (ids can change across a reboot while
/// the name stays).
pub fn resolve(want: &Option<DeviceRef>, avail: &[DeviceRef]) -> Resolved {
    let Some(w) = want else {
        return Resolved::Default;
    };
    if let Some(i) = avail.iter().position(|d| d.id == w.id) {
        return Resolved::Named(i);
    }
    if let Some(i) = avail.iter().position(|d| d.name == w.name) {
        return Resolved::Named(i);
    }
    Resolved::Missing(w.name.clone())
}

/// Input level, published by the capture path and sampled by the levels writer.
#[derive(Default)]
pub struct Levels {
    /// Largest |sample| since the last `take_peak`, as f32 bits.
    in_peak: AtomicU32,
    /// Sum of squares (f64 bits) and sample count since the last reading: the RMS is over
    /// that window (~33 ms), not one callback block, so it does not jitter with block size.
    in_sq: AtomicU64,
    in_n: AtomicU64,
    /// Samples at or above full scale since start.
    clips: AtomicU32,
    out_peak: AtomicU32,
}

fn max_bits(a: &AtomicU32, v: f32) {
    let mut cur = a.load(Ordering::Relaxed);
    while v > f32::from_bits(cur) {
        match a.compare_exchange_weak(cur, v.to_bits(), Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(c) => cur = c,
        }
    }
}

/// The live audio state shared by the callbacks, the levels writer and the settings watcher.
pub struct Audio {
    pub input: Option<DeviceRef>,
    pub output: Option<DeviceRef>,
    gain: AtomicU32,
    volume: AtomicU32,
    pub levels: Levels,
}

impl Audio {
    pub fn new(s: &Settings) -> Audio {
        Audio {
            input: s.input.clone(),
            output: s.output.clone(),
            gain: AtomicU32::new(db_to_linear(s.gain_db).to_bits()),
            volume: AtomicU32::new(s.volume.to_bits()),
            levels: Levels::default(),
        }
    }

    pub fn set_gain_db(&self, db: f32) {
        self.gain
            .store(db_to_linear(db.clamp(GAIN_DB_RANGE.0, GAIN_DB_RANGE.1)).to_bits(), Ordering::Relaxed);
    }

    pub fn set_volume(&self, v: f32) {
        self.volume.store(v.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
    }

    /// Gain, then the meter. Called on the capture path before the encoder, so the meter,
    /// the clip mark and the loopback test all see what other members are sent. Real-time
    /// safe: no allocation, no locks.
    pub fn condition_input(&self, buf: &mut [f32]) {
        let g = f32::from_bits(self.gain.load(Ordering::Relaxed));
        let (mut peak, mut sq, mut clips) = (0f32, 0f32, 0u32);
        for s in buf.iter_mut() {
            if g != 1.0 {
                *s *= g;
            }
            let a = s.abs();
            peak = peak.max(a);
            sq += *s * *s;
            if a >= 0.999 {
                clips += 1;
            }
            // Processing (noise suppression, AGC) would go here, per sample or per block.
        }
        if !buf.is_empty() {
            max_bits(&self.levels.in_peak, peak);
            let mut cur = self.levels.in_sq.load(Ordering::Relaxed);
            loop {
                let new = (f64::from_bits(cur) + sq as f64).to_bits();
                match self.levels.in_sq.compare_exchange_weak(cur, new, Ordering::Relaxed, Ordering::Relaxed) {
                    Ok(_) => break,
                    Err(c) => cur = c,
                }
            }
            self.levels.in_n.fetch_add(buf.len() as u64, Ordering::Relaxed);
            if clips > 0 {
                self.levels.clips.fetch_add(clips, Ordering::Relaxed);
            }
        }
    }

    /// Output volume and the output meter.
    pub fn condition_output(&self, buf: &mut [f32]) {
        let v = f32::from_bits(self.volume.load(Ordering::Relaxed));
        let mut peak = 0f32;
        for s in buf.iter_mut() {
            if v != 1.0 {
                *s *= v;
            }
            peak = peak.max(s.abs());
        }
        max_bits(&self.levels.out_peak, peak);
    }

    fn snapshot(&self) -> (f32, f32, u32, f32) {
        let peak = f32::from_bits(self.levels.in_peak.swap(0, Ordering::Relaxed));
        let n = self.levels.in_n.swap(0, Ordering::Relaxed);
        let sq = f64::from_bits(self.levels.in_sq.swap(0, Ordering::Relaxed));
        let rms = if n > 0 {
            let r = ((sq / n as f64) as f32).sqrt();
            r
        } else {
            // No samples this window (no source is running): silence.
            0.0
        };
        let clips = self.levels.clips.load(Ordering::Relaxed);
        let out = f32::from_bits(self.levels.out_peak.swap(0, Ordering::Relaxed));
        (peak, rms, clips, out)
    }
}

// ---- the levels file

pub const LEVELS_MAGIC: &[u8; 4] = b"LVL1";
pub const LEVELS_LEN: usize = 32;

/// One reading of the levels file.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LevelsReading {
    /// Increments on every write: unchanged for a while means the writer is gone.
    pub seq: u32,
    pub peak: f32,
    pub rms: f32,
    pub clips: u32,
    pub out_peak: f32,
}

pub fn read_levels(path: &Path) -> Option<LevelsReading> {
    let f = std::fs::File::open(path).ok()?;
    let mut b = [0u8; LEVELS_LEN];
    f.read_exact_at(&mut b, 0).ok()?;
    if &b[0..4] != LEVELS_MAGIC {
        return None;
    }
    let u = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    let f = |i: usize| f32::from_bits(u(i));
    Some(LevelsReading {
        seq: u(4),
        peak: f(8),
        rms: f(12),
        clips: u(16),
        out_peak: f(20),
    })
}

/// Rewrites the levels file ~30 times a second and re-reads gain and volume from the
/// settings file twice a second. Ends when `stop` is set.
pub fn spawn_levels_writer(
    audio: Arc<Audio>,
    path: Option<PathBuf>,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("voice-levels".into())
        .spawn(move || {
            let file = path.as_ref().and_then(|p| {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(p)
                    .ok()
            });
            let settings_path = Settings::path();
            let mut seq = 0u32;
            let mut tick = 0u32;
            let mut mtime = None;
            while !stop.load(Ordering::Relaxed) {
                if let Some(f) = &file {
                    let (peak, rms, clips, out) = audio.snapshot();
                    let mut b = [0u8; LEVELS_LEN];
                    b[0..4].copy_from_slice(LEVELS_MAGIC);
                    seq = seq.wrapping_add(1);
                    b[4..8].copy_from_slice(&seq.to_le_bytes());
                    b[8..12].copy_from_slice(&peak.to_bits().to_le_bytes());
                    b[12..16].copy_from_slice(&rms.to_bits().to_le_bytes());
                    b[16..20].copy_from_slice(&clips.to_le_bytes());
                    b[20..24].copy_from_slice(&out.to_bits().to_le_bytes());
                    let _ = f.write_all_at(&b, 0);
                }
                tick += 1;
                if tick % 15 == 0 {
                    let m = std::fs::metadata(&settings_path).and_then(|m| m.modified()).ok();
                    if m != mtime {
                        mtime = m;
                        if m.is_some() {
                            let s = Settings::load_from(&settings_path);
                            audio.set_gain_db(s.gain_db);
                            audio.set_volume(s.volume);
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(33));
            }
        })
        .expect("spawn levels writer")
}

/// Build the shared audio state from the saved settings and the command line.
pub fn from_flags(f: &dyn Flags) -> Result<Arc<Audio>> {
    let mut s = Settings::load();
    if let Some(v) = f.value("input-device") {
        s.input = Some(DeviceRef { id: v.to_string(), name: v.to_string() });
    }
    if let Some(v) = f.value("output-device") {
        s.output = Some(DeviceRef { id: v.to_string(), name: v.to_string() });
    }
    if let Some(v) = f.value("gain-db") {
        s.gain_db = v.parse().with_context(|| format!("--gain-db {v:?}"))?;
    }
    if let Some(v) = f.value("volume") {
        s.volume = v.parse().with_context(|| format!("--volume {v:?}"))?;
    }
    Ok(Arc::new(Audio::new(&s)))
}

// ---- devices (cpal)

#[cfg(feature = "device")]
pub mod dev {
    use super::*;
    use cpal::traits::{DeviceTrait, HostTrait};

    fn info(d: &cpal::Device) -> Option<DeviceRef> {
        Some(DeviceRef {
            id: d.id().ok()?.to_string(),
            name: d.description().ok()?.name().to_string(),
        })
    }

    pub struct Listing {
        pub inputs: Vec<DeviceRef>,
        pub outputs: Vec<DeviceRef>,
        pub default_input: Option<DeviceRef>,
        pub default_output: Option<DeviceRef>,
    }

    /// The ALSA backend lists dozens of plugin pseudo-devices (rate converters, upmixers,
    /// raw hw: nodes). Keep the ones a person would pick: the sound server entries and the
    /// per-card `sysdefault`. Other backends are listed as they come.
    fn tidy(v: Vec<DeviceRef>) -> Vec<DeviceRef> {
        if !v.iter().any(|d| d.id.starts_with("alsa:")) {
            return v;
        }
        v.into_iter()
            .filter(|d| matches!(d.id.as_str(), "alsa:default" | "alsa:pipewire" | "alsa:pulse") || d.id.starts_with("alsa:sysdefault:"))
            .collect()
    }

    /// Enumerates without opening any stream (no microphone is touched).
    pub fn list() -> Listing {
        let host = cpal::default_host();
        let inputs = host.input_devices().map(|i| i.filter_map(|d| info(&d)).collect()).unwrap_or_default();
        let outputs = host.output_devices().map(|i| i.filter_map(|d| info(&d)).collect()).unwrap_or_default();
        Listing {
            inputs: tidy(inputs),
            outputs: tidy(outputs),
            default_input: host.default_input_device().and_then(|d| info(&d)),
            default_output: host.default_output_device().and_then(|d| info(&d)),
        }
    }

    /// The device to open, and whether a saved choice was missing (so the default is used).
    pub fn pick(input: bool, want: &Option<DeviceRef>) -> Result<(cpal::Device, Option<String>)> {
        let host = cpal::default_host();
        let devices: Vec<cpal::Device> = if input {
            host.input_devices().map(|i| i.collect()).unwrap_or_default()
        } else {
            host.output_devices().map(|i| i.collect()).unwrap_or_default()
        };
        let avail: Vec<DeviceRef> = devices.iter().filter_map(info).collect();
        let default = || {
            if input { host.default_input_device() } else { host.default_output_device() }
                .context(if input { "no default input device" } else { "no default output device" })
        };
        Ok(match resolve(want, &avail) {
            Resolved::Default => (default()?, None),
            Resolved::Named(i) => {
                let id = &avail[i].id;
                let d = devices
                    .into_iter()
                    .find(|d| d.id().ok().map(|x| x.to_string()).as_ref() == Some(id))
                    .context("device vanished")?;
                (d, None)
            }
            Resolved::Missing(name) => (default()?, Some(name)),
        })
    }
}

/// `prim voice devices [--json]`
pub fn run_devices(_f: &dyn Flags) -> Result<()> {
    #[cfg(feature = "device")]
    {
        let l = dev::list();
        let j = |d: &DeviceRef| json!({"id": d.id, "name": d.name});
        let v = json!({
            "inputs": l.inputs.iter().map(j).collect::<Vec<_>>(),
            "outputs": l.outputs.iter().map(j).collect::<Vec<_>>(),
            "default_input": l.default_input.as_ref().map(j),
            "default_output": l.default_output.as_ref().map(j),
        });
        println!("{v}");
        Ok(())
    }
    #[cfg(not(feature = "device"))]
    bail!("this build has no audio devices (feature `device` is off)")
}

// ---- sources and sinks shared by `meter` and `test`

/// Mono samples from the chosen source into `ring`, with `audio.condition_input` applied.
/// Returns the source rate. The stream (if any) lives in the returned guard.
pub struct Source {
    pub rate: u32,
    _guard: Box<dyn std::any::Any>,
    stop: Arc<AtomicBool>,
}

impl Drop for Source {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

pub fn open_source(f: &dyn Flags, audio: &Arc<Audio>, ring: &Arc<Ring>) -> Result<Source> {
    use crate::capture;
    use crate::config::CaptureSpec;
    let spec = f.value("capture").unwrap_or("device");
    let stop = Arc::new(AtomicBool::new(false));
    if spec == "device" {
        #[cfg(feature = "device")]
        {
            let (guard, rate) = open_cpal_input(audio, ring)?;
            return Ok(Source { rate, _guard: Box::new(guard), stop });
        }
        #[cfg(not(feature = "device"))]
        bail!("this build has no audio devices (feature `device` is off)");
    }
    let cs = crate::config::parse_capture(spec)?;
    let mut gen = capture::generator(&cs, 1)?.with_context(|| format!("--capture {spec}: not a source"))?;
    if matches!(cs, CaptureSpec::None) {
        bail!("--capture none has nothing to record");
    }
    let rate = gen.rate();
    let (a, r, st) = (audio.clone(), ring.clone(), stop.clone());
    let h = std::thread::spawn(move || {
        let t0 = Instant::now();
        let mut produced = 0u64;
        let mut buf = vec![0f32; 4096];
        while !st.load(Ordering::Relaxed) {
            let due = (t0.elapsed().as_secs_f64() * rate as f64) as u64;
            while produced < due {
                let n = ((due - produced) as usize).min(buf.len());
                gen.fill(&mut buf[..n]);
                a.condition_input(&mut buf[..n]);
                r.push(&buf[..n]);
                produced += n as u64;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    Ok(Source { rate, _guard: Box::new(h), stop })
}

#[cfg(feature = "device")]
fn open_cpal_input(audio: &Arc<Audio>, ring: &Arc<Ring>) -> Result<(cpal::Stream, u32)> {
    use cpal::traits::{DeviceTrait, StreamTrait};
    use cpal::SampleFormat;
    let (device, missing) = dev::pick(true, &audio.input)?;
    if let Some(n) = missing {
        eprintln!("prim voice: saved microphone {n:?} is not connected; using the system default");
    }
    let sc = device.default_input_config().context("input config")?;
    let (fmt, channels, rate) = (sc.sample_format(), sc.channels() as usize, sc.sample_rate());
    let cfg: cpal::StreamConfig = sc.into();
    fn build<T>(d: &cpal::Device, cfg: cpal::StreamConfig, ch: usize, a: Arc<Audio>, r: Arc<Ring>) -> Result<cpal::Stream, cpal::Error>
    where
        T: cpal::SizedSample,
        f32: cpal::FromSample<T>,
    {
        d.build_input_stream(
            cfg,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                let mut mono = [0f32; 1024];
                for chunk in data.chunks(ch * mono.len()) {
                    let frames = chunk.len() / ch;
                    for (i, f) in chunk.chunks(ch).take(frames).enumerate() {
                        mono[i] = f.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() / ch as f32;
                    }
                    a.condition_input(&mut mono[..frames]);
                    r.push(&mono[..frames]);
                }
            },
            |_e| {},
            None,
        )
    }
    let (a, r) = (audio.clone(), ring.clone());
    let stream = match fmt {
        SampleFormat::F32 => build::<f32>(&device, cfg, channels, a, r),
        SampleFormat::I16 => build::<i16>(&device, cfg, channels, a, r),
        SampleFormat::I32 => build::<i32>(&device, cfg, channels, a, r),
        SampleFormat::U16 => build::<u16>(&device, cfg, channels, a, r),
        other => bail!("input sample format {other} is not supported"),
    }
    .context("opening the input stream")?;
    stream.play().context("starting the input stream")?;
    Ok((stream, rate))
}

/// Where the loopback test plays back: the selected speakers, or a test sink.
pub enum Playback {
    Wav(hound::WavWriter<std::io::BufWriter<std::fs::File>>),
    Null,
    #[cfg(feature = "device")]
    Device(Arc<Ring>, cpal::Stream),
}

impl Playback {
    pub fn open(f: &dyn Flags, audio: &Arc<Audio>) -> Result<Playback> {
        let spec = f.value("output").unwrap_or("device");
        Ok(match spec {
            "null" => Playback::Null,
            "device" => {
                #[cfg(feature = "device")]
                {
                    use cpal::traits::{DeviceTrait, StreamTrait};
                    let (device, missing) = dev::pick(false, &audio.output)?;
                    if let Some(n) = missing {
                        eprintln!("prim voice: saved speakers {n:?} are not connected; using the system default");
                    }
                    let ring = Ring::new(48_000 * 2);
                    let r = ring.clone();
                    let cfg = cpal::StreamConfig {
                        channels: 2,
                        sample_rate: crate::shared::SAMPLE_RATE,
                        buffer_size: cpal::BufferSize::Default,
                    };
                    let stream = device
                        .build_output_stream(
                            cfg,
                            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                                let got = r.pop(data);
                                data[got..].fill(0.0);
                            },
                            |_e| {},
                            None,
                        )
                        .context("opening the output stream (48 kHz stereo f32)")?;
                    stream.play().context("starting the output stream")?;
                    Playback::Device(ring, stream)
                }
                #[cfg(not(feature = "device"))]
                bail!("this build has no audio devices (feature `device` is off)")
            }
            p => {
                let path = p.strip_prefix("wav:").with_context(|| format!("--output {p:?}: expected device, wav:PATH or null"))?;
                Playback::Wav(hound::WavWriter::create(
                    path,
                    hound::WavSpec { channels: 2, sample_rate: 48_000, bits_per_sample: 32, sample_format: hound::SampleFormat::Float },
                )?)
            }
        })
    }

    /// Mono 48 kHz in; stereo out through the output volume.
    pub fn write(&mut self, audio: &Audio, mono: &[f32]) {
        let mut st: Vec<f32> = mono.iter().flat_map(|s| [*s, *s]).collect();
        audio.condition_output(&mut st);
        match self {
            Playback::Null => {}
            Playback::Wav(w) => {
                for s in &st {
                    let _ = w.write_sample(*s);
                }
            }
            #[cfg(feature = "device")]
            Playback::Device(r, _) => {
                let mut off = 0;
                while off < st.len() {
                    off += r.push(&st[off..]);
                    if off < st.len() {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
            }
        }
    }

    pub fn finish(self) {
        if let Playback::Wav(w) = self {
            let _ = w.finalize();
        }
    }
}

fn emit(v: Value) {
    use std::io::Write;
    println!("{v}");
    let _ = std::io::stdout().flush();
}

fn term() -> bool {
    crate::signals::TERM.load(Ordering::SeqCst)
}

/// `prim voice meter --levels PATH [--capture SRC]`: read the selected microphone through
/// the same gain stage as voice and publish its level, until stopped. No room, no lease.
pub fn run_meter(f: &dyn Flags) -> Result<()> {
    let audio = from_flags(f)?;
    let ring = Ring::new(48_000);
    let path = f.value("levels").map(PathBuf::from).context("meter needs --levels PATH")?;
    let src = open_source(f, &audio, &ring)?;
    let stop = Arc::new(AtomicBool::new(false));
    let _w = spawn_levels_writer(audio.clone(), Some(path), stop.clone());
    let duration = f.value("duration").map(|v| v.parse::<f64>()).transpose()?;
    let t0 = Instant::now();
    let mut sink = vec![0f32; 4096];
    emit(json!({"phase": "metering", "rate": src.rate}));
    while !term() && duration.is_none_or(|d| t0.elapsed().as_secs_f64() < d) {
        while ring.pop(&mut sink) > 0 {} // nobody needs the samples; keep the ring from filling
        std::thread::sleep(Duration::from_millis(20));
    }
    stop.store(true, Ordering::SeqCst);
    Ok(())
}

/// The record side of the test, then through the real send path (same Opus encoder, 16 kbps,
/// DTX), then the real receive path (NetEq), to the selected speakers. Prints JSON lines
/// for the window: recording progress, playing, done (with sizes), or an error.
pub fn run_test(f: &dyn Flags) -> Result<()> {
    use voice_core::{PacketObservation, PlayoutReceiver, VoiceEncoder, VoiceEncoderConfig};
    let audio = from_flags(f)?;
    let secs: f64 = f.value("seconds").map(str::parse).transpose().context("--seconds")?.unwrap_or(5.0);
    let ring = Ring::new(48_000 * 2);
    let src = open_source(f, &audio, &ring)?;
    let stop = Arc::new(AtomicBool::new(false));
    let _w = spawn_levels_writer(audio.clone(), f.value("levels").map(PathBuf::from), stop.clone());

    // Record. Samples go through the encoder as they arrive, as in a live send.
    let rate = src.rate.max(8000);
    let mut enc = VoiceEncoder::new(VoiceEncoderConfig { input_sample_rate: rate, enable_dtx: !f.has("no-dtx"), ..Default::default() })
        .map_err(|e| anyhow::anyhow!("opus encoder: {e}"))?;
    let mut packets: Vec<(u64, voice_core::VoicePacket)> = vec![];
    let mut recorded = 0u64;
    let t0 = Instant::now();
    let mut tmp = vec![0f32; 4096];
    let mut last_emit = -1.0;
    while t0.elapsed().as_secs_f64() < secs && !term() {
        let n = ring.pop(&mut tmp);
        if n > 0 {
            enc.push_pcm(&tmp[..n]);
            recorded += n as u64;
            let at = (recorded * 1_000_000 / rate as u64).saturating_sub(20_000);
            for _ in 0..crate::send::poll_budget(n, rate) {
                match enc.poll_packet() {
                    Ok(Some(p)) => packets.push((at, p)),
                    Ok(None) => {}
                    Err(_) => break,
                }
            }
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
        let t = (t0.elapsed().as_secs_f64() * 10.0).floor() / 10.0;
        if t != last_emit {
            last_emit = t;
            emit(json!({"phase": "recording", "t": t, "of": secs}));
        }
    }
    if let Some(p) = enc.finish_talkspurt() {
        packets.push((recorded * 1_000_000 / rate as u64, p));
    }
    drop(src);
    if term() {
        return Ok(());
    }
    let sent_bytes: usize = packets.iter().map(|(_, p)| p.payload.len()).sum();
    if packets.iter().all(|(_, p)| p.payload.is_empty()) {
        emit(json!({"phase": "done", "packets": packets.len(), "bytes": 0, "silent": true}));
        return Ok(());
    }

    // Play back through the receive path in real time, as a friend's client would hear it.
    emit(json!({"phase": "playing", "of": secs}));
    let mut play = Playback::open(f, &audio)?;
    let mut rx = PlayoutReceiver::new().map_err(|e| anyhow::anyhow!("receiver: {e}"))?;
    let start = Instant::now();
    let mut next = 0usize;
    let mut frame = vec![0f32; 480]; // 10 ms at 48 kHz mono
    let mut frames = 0u64;
    let total_frames = ((secs + 0.6) * 100.0) as u64;
    while frames < total_frames && !term() {
        let due = start.elapsed().as_micros() as u64;
        while next < packets.len() && packets[next].0 <= due {
            let (_, p) = &packets[next];
            let t = crate::clock::mono_us();
            let _ = rx.push_packet(p.clone(), PacketObservation { daemon_arrival_us: t, local_us: t });
            next += 1;
        }
        rx.pull(&mut frame);
        play.write(&audio, &frame);
        frames += 1;
        let target = Duration::from_millis(frames * 10);
        if let Some(wait) = target.checked_sub(start.elapsed()) {
            std::thread::sleep(wait);
        }
    }
    play.finish();
    stop.store(true, Ordering::SeqCst);
    emit(json!({"phase": "done", "packets": packets.len(), "bytes": sent_bytes}));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(id: &str, name: &str) -> DeviceRef {
        DeviceRef { id: id.into(), name: name.into() }
    }

    #[test]
    fn resolve_prefers_id_then_name_then_reports_missing() {
        let avail = vec![d("a", "Mic A"), d("b", "Mic B")];
        assert_eq!(resolve(&None, &avail), Resolved::Default);
        assert_eq!(resolve(&Some(d("b", "x")), &avail), Resolved::Named(1));
        assert_eq!(resolve(&Some(d("zz", "Mic A")), &avail), Resolved::Named(0));
        assert_eq!(resolve(&Some(d("zz", "Gone")), &avail), Resolved::Missing("Gone".into()));
    }

    #[test]
    fn settings_round_trip_and_clamp() {
        let dir = tempdir();
        let p = dir.join("audio.json");
        let s = Settings { input: Some(d("i", "In")), output: None, gain_db: 6.5, volume: 0.5 };
        s.save_to(&p).unwrap();
        assert_eq!(Settings::load_from(&p), s);
        std::fs::write(&p, r#"{"gain_db": 99, "volume": -3}"#).unwrap();
        let l = Settings::load_from(&p);
        assert_eq!((l.gain_db, l.volume), (GAIN_DB_RANGE.1, 0.0));
        assert_eq!(Settings::load_from(&dir.join("missing.json")), Settings::default());
    }

    #[test]
    fn gain_applies_before_the_meter_and_clips_are_counted() {
        let a = Audio::new(&Settings { gain_db: 6.0, ..Default::default() });
        let mut b = vec![0.25f32; 480];
        a.condition_input(&mut b);
        assert!((b[0] - 0.25 * db_to_linear(6.0)).abs() < 1e-6);
        let (peak, rms, clips, _) = a.snapshot();
        assert!((peak - b[0]).abs() < 1e-6, "meter sees the amplified signal");
        assert!((rms - b[0]).abs() < 1e-5);
        assert_eq!(clips, 0);
        let mut hot = vec![0.9f32; 10];
        a.condition_input(&mut hot);
        assert_eq!(a.snapshot().2, 10, "clipping counted");
    }

    fn tempdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("prim-audio-test-{}-{}", std::process::id(), crate::clock::mono_us()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}
