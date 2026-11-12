//! Output sinks that need a clock: WAV, null and in-memory capture, paced by
//! CLOCK_MONOTONIC the way a device callback is paced by its hardware. (The device sink
//! lives in `device.rs`.) The pacer pops exactly what the wall clock says is due, writing
//! silence and counting an underrun when the mixer hasn't delivered.

use std::io::BufWriter;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;

use crate::clock::mono_us;
use crate::config::OutputSpec;
use crate::shared::{Shared, SAMPLE_RATE};

enum Sink {
    Null,
    Wav(hound::WavWriter<BufWriter<std::fs::File>>),
    Capture(crate::config::OutputBuffer),
}

pub fn needs_pacer(spec: &OutputSpec) -> bool {
    !matches!(spec, OutputSpec::Device)
}

pub fn spawn_pacer(sh: Arc<Shared>) -> Result<std::thread::JoinHandle<()>> {
    let mut sink = match &sh.cfg.output {
        OutputSpec::Null => Sink::Null,
        OutputSpec::Capture(b) => Sink::Capture(b.clone()),
        OutputSpec::Wav(p) => Sink::Wav(hound::WavWriter::create(
            p,
            hound::WavSpec {
                channels: 2,
                sample_rate: SAMPLE_RATE,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )?),
        OutputSpec::Device => unreachable!("device output has no pacer"),
    };
    Ok(std::thread::Builder::new()
        .name("voice-out-pacer".into())
        .spawn(move || {
            // Start the clock once the mixer has its first blocks ready.
            while !sh.primed.load(Ordering::SeqCst) {
                if sh.is_shutdown() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            let t0 = mono_us();
            let mut consumed: u64 = 0;
            let mut last = 0u64;
            let mut buf = vec![0f32; 2 * 4800];
            while !sh.is_shutdown() {
                let now = mono_us();
                if last != 0 {
                    sh.stats.callback_gap_us.record(now - last);
                }
                last = now;
                let due = (now - t0) * SAMPLE_RATE as u64 / 1_000_000;
                while consumed < due {
                    let frames = ((due - consumed) as usize).min(4800);
                    let want = frames * 2;
                    let got = sh.out_ring.pop(&mut buf[..want]);
                    if got < want {
                        buf[got..want].fill(0.0);
                        sh.stats.underruns.fetch_add(1, Ordering::Relaxed);
                    }
                    write(&mut sink, &buf[..want]);
                    consumed += frames as u64;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            if let Sink::Wav(w) = sink {
                let _ = w.finalize();
            }
        })?)
}

fn write(sink: &mut Sink, data: &[f32]) {
    match sink {
        Sink::Null => {}
        Sink::Wav(w) => {
            for s in data {
                let _ = w.write_sample(*s);
            }
        }
        Sink::Capture(b) => b.lock().unwrap().extend_from_slice(data),
    }
}
