//! Capture sources. Each fills the shared mono capture ring; the encoder thread drains
//! it. Synthetic sources are paced by CLOCK_MONOTONIC, optionally skewed by N ppm to
//! emulate a device whose sample clock runs fast or slow.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::clock::mono_us;
use crate::config::CaptureSpec;
use crate::shared::{Shared, SAMPLE_RATE};
use crate::tone;

/// A synthetic sample source at `rate`.
pub trait Generator: Send {
    fn rate(&self) -> u32;
    fn fill(&mut self, out: &mut [f32]);
}

pub struct SineGen {
    hz: f64,
    phase: f64,
    amp: f32,
}

impl SineGen {
    pub fn new(hz: f32, amp: f32) -> SineGen {
        SineGen {
            hz: hz as f64,
            phase: 0.0,
            amp,
        }
    }
}

impl Generator for SineGen {
    fn rate(&self) -> u32 {
        SAMPLE_RATE
    }

    fn fill(&mut self, out: &mut [f32]) {
        let step = std::f64::consts::TAU * self.hz / SAMPLE_RATE as f64;
        for o in out.iter_mut() {
            *o = self.amp * self.phase.sin() as f32;
            self.phase += step;
            if self.phase > std::f64::consts::TAU {
                self.phase -= std::f64::consts::TAU;
            }
        }
    }
}

pub struct WavGen {
    samples: Vec<f32>,
    pos: usize,
    rate: u32,
}

impl WavGen {
    pub fn open(path: &Path) -> Result<WavGen> {
        let mut r =
            hound::WavReader::open(path).with_context(|| format!("opening {}", path.display()))?;
        let spec = r.spec();
        let ch = spec.channels.max(1) as usize;
        let raw: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Float => r.samples::<f32>().collect::<Result<_, _>>()?,
            hound::SampleFormat::Int => {
                let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
                r.samples::<i32>()
                    .map(|s| s.map(|v| v as f32 * scale))
                    .collect::<Result<_, _>>()?
            }
        };
        let samples: Vec<f32> = raw
            .chunks(ch)
            .map(|f| f.iter().sum::<f32>() / ch as f32)
            .collect();
        anyhow::ensure!(!samples.is_empty(), "{} has no samples", path.display());
        Ok(WavGen {
            samples,
            pos: 0,
            rate: spec.sample_rate,
        })
    }
}

impl Generator for WavGen {
    fn rate(&self) -> u32 {
        self.rate
    }

    fn fill(&mut self, out: &mut [f32]) {
        for o in out.iter_mut() {
            *o = self.samples[self.pos];
            self.pos = (self.pos + 1) % self.samples.len();
        }
    }
}

/// Build the generator for a synthetic spec (None for `none` and `device`).
pub fn generator(spec: &CaptureSpec, incarnation: u32) -> Result<Option<Box<dyn Generator>>> {
    Ok(match spec {
        CaptureSpec::None | CaptureSpec::Device => None,
        CaptureSpec::Tone(hz) => Some(Box::new(SineGen::new(*hz, 0.3))),
        CaptureSpec::ToneAuto => Some(Box::new(SineGen::new(tone::auto_hz(incarnation), 0.3))),
        CaptureSpec::Wav(p) => Some(Box::new(WavGen::open(p)?)),
    })
}

/// Start the pacing thread: pushes `rate * (1 + skew)` samples per wall second.
pub fn spawn_synthetic(
    sh: Arc<Shared>,
    mut gen: Box<dyn Generator>,
) -> std::thread::JoinHandle<()> {
    let skew = 1.0 + sh.cfg.skew_ppm * 1e-6;
    let rate = gen.rate();
    sh.gen.capture_rate.store(rate, Ordering::SeqCst);
    std::thread::Builder::new()
        .name("voice-capture-synth".into())
        .spawn(move || {
            let t0 = mono_us();
            let mut produced: u64 = 0;
            let mut buf = vec![0f32; 4096];
            while !sh.is_shutdown() {
                let due = ((mono_us() - t0) as f64 * 1e-6 * rate as f64 * skew) as u64;
                while produced < due {
                    let n = ((due - produced) as usize).min(buf.len());
                    gen.fill(&mut buf[..n]);
                    // A full ring means the encoder fell behind; the samples are lost,
                    // as a device's overrun would lose them.
                    sh.cap_ring.push(&buf[..n]);
                    produced += n as u64;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        })
        .expect("spawn capture")
}
