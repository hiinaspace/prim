//! Audio devices through cpal (ALSA, so PipeWire's ALSA plugin on a desktop). The
//! callbacks only copy between the preallocated rings and touch atomics: no allocation,
//! no contended lock, no logging, no IPC.

use std::sync::Arc;
#[cfg(feature = "device")]
use std::sync::atomic::Ordering;

use anyhow::Result;

use crate::shared::Shared;

/// Holds the open streams (dropping it closes them).
pub struct Streams {
    #[cfg(feature = "device")]
    _input: Option<cpal::Stream>,
    #[cfg(feature = "device")]
    _output: Option<cpal::Stream>,
}

#[cfg(not(feature = "device"))]
pub fn open(_sh: &Arc<Shared>) -> Result<Streams> {
    Ok(Streams {})
}

#[cfg(feature = "device")]
pub fn open(sh: &Arc<Shared>) -> Result<Streams> {
    use crate::config::{CaptureSpec, OutputSpec};
    let input = if matches!(sh.cfg.capture, CaptureSpec::Device) {
        Some(open_input(sh)?)
    } else {
        None
    };
    let output = if matches!(sh.cfg.output, OutputSpec::Device) {
        Some(open_output(sh)?)
    } else {
        None
    };
    Ok(Streams {
        _input: input,
        _output: output,
    })
}

/// A stream error that means "rebuild on the current default device".
#[cfg(feature = "device")]
fn error_callback(sh: &Arc<Shared>) -> impl FnMut(cpal::Error) + Send + 'static {
    let sh = sh.clone();
    move |e: cpal::Error| {
        use cpal::ErrorKind::*;
        if matches!(e.kind(), DeviceNotAvailable | StreamInvalidated) {
            sh.gen.reopen.store(true, Ordering::SeqCst);
        }
    }
}

#[cfg(feature = "device")]
fn open_output(sh: &Arc<Shared>) -> Result<cpal::Stream> {
    use anyhow::Context;
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let dev = cpal::default_host()
        .default_output_device()
        .context("no default output device")?;
    let cfg = cpal::StreamConfig {
        channels: 2,
        sample_rate: crate::shared::SAMPLE_RATE,
        buffer_size: cpal::BufferSize::Default,
    };
    let ring = sh.out_ring.clone();
    let cb = sh.clone();
    let mut last_us = 0u64;
    let stream = dev
        .build_output_stream(
            cfg,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let now = crate::clock::mono_us();
                if last_us != 0 {
                    cb.stats.callback_gap_us.record(now - last_us);
                }
                last_us = now;
                let got = ring.pop(data);
                if got < data.len() {
                    data[got..].fill(0.0);
                    if cb.primed.load(Ordering::Relaxed) {
                        cb.stats.underruns.fetch_add(1, Ordering::Relaxed);
                    }
                }
                for s in data.iter_mut() {
                    *s = s.clamp(-1.0, 1.0);
                }
            },
            error_callback(sh),
            None,
        )
        .context("opening the output stream (48 kHz stereo f32)")?;
    stream.play().context("starting the output stream")?;
    Ok(stream)
}

#[cfg(feature = "device")]
fn open_input(sh: &Arc<Shared>) -> Result<cpal::Stream> {
    use anyhow::Context;
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::SampleFormat;

    let dev = cpal::default_host()
        .default_input_device()
        .context("no default input device")?;
    let sc = dev.default_input_config().context("input config")?;
    let (fmt, channels, rate) = (sc.sample_format(), sc.channels() as usize, sc.sample_rate());
    let cfg: cpal::StreamConfig = sc.into();
    // The encoder reads the rate before the first sample can exist.
    sh.gen.capture_rate.store(rate, Ordering::SeqCst);
    let stream = match fmt {
        SampleFormat::F32 => input_stream::<f32>(&dev, cfg, channels, sh),
        SampleFormat::I16 => input_stream::<i16>(&dev, cfg, channels, sh),
        SampleFormat::I32 => input_stream::<i32>(&dev, cfg, channels, sh),
        SampleFormat::U16 => input_stream::<u16>(&dev, cfg, channels, sh),
        other => anyhow::bail!("input sample format {other} is not supported"),
    }
    .context("opening the input stream")?;
    stream.play().context("starting the input stream")?;
    Ok(stream)
}

#[cfg(feature = "device")]
fn input_stream<T>(
    dev: &cpal::Device,
    cfg: cpal::StreamConfig,
    channels: usize,
    sh: &Arc<Shared>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    use cpal::traits::DeviceTrait;
    let ring = sh.cap_ring.clone();
    dev.build_input_stream(
        cfg,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            // Downmix to mono through a stack buffer, in chunks.
            let mut mono = [0f32; 1024];
            for chunk in data.chunks(channels * mono.len()) {
                let frames = chunk.len() / channels;
                for (i, f) in chunk.chunks(channels).take(frames).enumerate() {
                    let mut acc = 0.0f32;
                    for s in f {
                        acc += s.to_sample::<f32>();
                    }
                    mono[i] = acc / channels as f32;
                }
                // A full ring drops samples, as an overrun would.
                ring.push(&mono[..frames]);
            }
        },
        error_callback(sh),
        None,
    )
}
