//! Tones: the `tone:auto` frequency map and Goertzel detection (the stale-buffer oracle).

/// A frequency for an incarnation, so a listener can tell incarnations apart. 20 Hz
/// steps between 300 Hz and 2280 Hz keep every tone on its own 1 Hz Goertzel bin and
/// well inside what 16 kbit/s Opus carries.
pub fn auto_hz(incarnation: u32) -> f32 {
    300.0 + 20.0 * ((incarnation.wrapping_mul(0x9E37_79B1) >> 16) % 100) as f32
}

/// Amplitude of the `hz` component of `x` (a sine of amplitude A reads as A when it
/// fills the window), via the Goertzel recurrence.
pub fn goertzel_amp(x: &[f32], hz: f32, sample_rate: f32) -> f32 {
    let mut g = Goertzel::new(hz, sample_rate);
    for &s in x {
        g.push(s);
    }
    g.amplitude()
}

#[derive(Clone, Debug)]
pub struct Goertzel {
    coeff: f64,
    s1: f64,
    s2: f64,
    n: u64,
}

impl Goertzel {
    pub fn new(hz: f32, sample_rate: f32) -> Goertzel {
        let w = 2.0 * std::f64::consts::PI * hz as f64 / sample_rate as f64;
        Goertzel {
            coeff: 2.0 * w.cos(),
            s1: 0.0,
            s2: 0.0,
            n: 0,
        }
    }

    pub fn push(&mut self, x: f32) {
        let s = x as f64 + self.coeff * self.s1 - self.s2;
        self.s2 = self.s1;
        self.s1 = s;
        self.n += 1;
    }

    pub fn amplitude(&self) -> f32 {
        if self.n == 0 {
            return 0.0;
        }
        let p = self.s1 * self.s1 + self.s2 * self.s2 - self.coeff * self.s1 * self.s2;
        (2.0 * p.max(0.0).sqrt() / self.n as f64) as f32
    }

    pub fn reset(&mut self) {
        self.s1 = 0.0;
        self.s2 = 0.0;
        self.n = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_amplitude_and_rejects_other_tones() {
        let x: Vec<f32> = (0..48000)
            .map(|i| 0.25 * (2.0 * std::f32::consts::PI * 600.0 * i as f32 / 48000.0).sin())
            .collect();
        assert!((goertzel_amp(&x, 600.0, 48000.0) - 0.25).abs() < 0.005);
        assert!(goertzel_amp(&x, 640.0, 48000.0) < 0.005);
        assert_ne!(auto_hz(1), auto_hz(2));
        assert!((300.0..2300.0).contains(&auto_hz(0xdead_beef)));
    }
}
