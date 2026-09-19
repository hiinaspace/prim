// SPDX-License-Identifier: Apache-2.0
// Adapted from Basis OpenLipSync; see native/viseme-model/NOTICE.md.
//! OpenLipSync's paired raw-dB HTK mel frontend. See native/viseme-model/NOTICE.md.
use rustfft::{num_complex::Complex, Fft, FftPlanner};
use std::{collections::VecDeque, f64::consts::PI, sync::Arc};

pub struct Frontend {
    resampler: Resampler,
    pending: VecDeque<f32>,
    history: [f32; 400],
    window: [f32; 400],
    filters: Vec<Vec<(usize, f32)>>,
    fft: Arc<dyn Fft<f32>>,
    buffer: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
}
impl Frontend {
    pub fn new(rate: u32) -> Self {
        let fft = FftPlanner::new().plan_fft_forward(1024);
        let hz_mel = |f: f32| 2595.0 * (1.0 + f / 700.0).log10();
        let lo = hz_mel(50.0);
        let hi = hz_mel(8000.0);
        let edges: Vec<f32> = (0..82)
            .map(|i| {
                let mel = lo + (hi - lo) * i as f32 / 81.0;
                1025.0 * 700.0 * (10.0_f32.powf(mel / 2595.0) - 1.0) / 16000.0
            })
            .collect();
        let filters = (0..80)
            .map(|m| {
                (0..513)
                    .filter_map(|k| {
                        let bin = k as f32;
                        let w = if bin >= edges[m] && bin <= edges[m + 1] {
                            (bin - edges[m]) / (edges[m + 1] - edges[m])
                        } else if bin > edges[m + 1] && bin <= edges[m + 2] {
                            (edges[m + 2] - bin) / (edges[m + 2] - edges[m + 1])
                        } else {
                            0.0
                        };
                        (w > 0.0).then_some((k, w))
                    })
                    .collect()
            })
            .collect();
        Self {
            resampler: Resampler::new(rate),
            pending: VecDeque::new(),
            history: [0.0; 400],
            window: std::array::from_fn(|i| {
                0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / 399.0).cos())
            }),
            filters,
            buffer: vec![Complex::default(); 1024],
            scratch: vec![Complex::default(); fft.get_inplace_scratch_len()],
            fft,
        }
    }
    pub fn reset(&mut self) {
        self.resampler.reset();
        self.pending.clear();
        self.history.fill(0.0);
    }
    pub fn push(&mut self, samples: &[f32]) {
        self.resampler.push(samples, &mut self.pending);
    }
    pub fn next_mel(&mut self) -> Option<[f32; 80]> {
        if self.pending.len() < 160 {
            return None;
        }
        self.history.copy_within(160.., 0);
        for x in &mut self.history[240..] {
            *x = self.pending.pop_front().unwrap();
        }
        self.buffer.fill(Complex::default());
        for (i, x) in self.history.iter().enumerate() {
            self.buffer[i].re = x * self.window[i];
        }
        self.fft
            .process_with_scratch(&mut self.buffer, &mut self.scratch);
        Some(std::array::from_fn(|m| {
            let power: f32 = self.filters[m]
                .iter()
                .map(|&(k, w)| self.buffer[k].norm_sqr() * w)
                .sum();
            10.0 * power.max(1e-10).log10()
        }))
    }
}
struct Resampler {
    rate: u32,
    table: Vec<f32>,
    buffer: VecDeque<f32>,
    time: u64,
}
impl Resampler {
    fn new(rate: u32) -> Self {
        let fc = 0.5 * (16000.0 / rate as f64).min(1.0) * 0.9;
        let mut table = vec![0.0; 1024 * 48];
        for p in 0..1024 {
            let mut sum = 0.0;
            for n in 0..48 {
                let t = n as f64 - 23.0 - p as f64 / 1024.0;
                let x = 2.0 * fc * t;
                let sinc = if x == 0.0 {
                    1.0
                } else {
                    (PI * x).sin() / (PI * x)
                };
                let w = 0.42 - 0.5 * (2.0 * PI * n as f64 / 47.0).cos()
                    + 0.08 * (4.0 * PI * n as f64 / 47.0).cos();
                let h = 2.0 * fc * sinc * w;
                table[p * 48 + n] = h as f32;
                sum += h;
            }
            for v in &mut table[p * 48..(p + 1) * 48] {
                *v *= (1.0 / sum) as f32;
            }
        }
        let mut s = Self {
            rate,
            table,
            buffer: VecDeque::new(),
            time: 23 * 16000,
        };
        s.reset();
        s
    }
    fn reset(&mut self) {
        self.buffer.clear();
        self.buffer.extend([0.0; 24]);
        self.time = 23 * 16000;
    }
    fn push(&mut self, input: &[f32], output: &mut VecDeque<f32>) {
        if self.rate == 16000 {
            output.extend(input.iter().copied());
            return;
        }
        self.buffer.extend(input.iter().copied());
        while self.time / 16000 + 24 < self.buffer.len() as u64 {
            let center = (self.time / 16000) as usize;
            let phase =
                (((self.time % 16000) as f64 * 1024.0 / 16000.0).round_ties_even() as usize) % 1024;
            let value = (0..48)
                .map(|n| self.buffer[center - 23 + n] * self.table[phase * 48 + n])
                .sum();
            output.push_back(value);
            self.time += self.rate as u64;
        }
        let remove = ((self.time / 16000) as usize)
            .saturating_sub(23)
            .min(self.buffer.len());
        self.buffer.drain(..remove);
        self.time -= remove as u64 * 16000;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn analyze(rate: u32, chunk: usize) -> Vec<[f32; 80]> {
        let mut f = Frontend::new(rate);
        let mut out = Vec::new();
        let samples: Vec<f32> = (0..rate).map(|i| (i as f32 * 0.073).sin() * 0.2).collect();
        for block in samples.chunks(chunk) {
            f.push(block);
            while let Some(m) = f.next_mel() {
                out.push(m);
            }
        }
        out
    }
    #[test]
    fn chunk_independent_and_reset() {
        for rate in [16000, 44100, 48000] {
            assert_eq!(analyze(rate, 127), analyze(rate, 960));
        }
        let mut f = Frontend::new(16000);
        f.push(&[0.7; 480]);
        let _ = f.next_mel();
        f.reset();
        f.push(&[0.0; 160]);
        assert_eq!(f.next_mel().unwrap(), [-100.0; 80]);
    }
}
