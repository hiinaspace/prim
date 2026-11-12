//! Histograms for the stats file: log2 µs buckets for shape, plus exact percentiles
//! from a bounded reservoir (exact until it fills, a uniform sample after).

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};

pub const BUCKETS: usize = 40;
const RESERVOIR: usize = 262_144;

/// Bucket 0 holds 0; bucket i (i ≥ 1) holds values in [2^(i-1), 2^i).
pub fn bucket(v: u64) -> usize {
    ((64 - v.leading_zeros()) as usize).min(BUCKETS - 1)
}

#[derive(Clone)]
pub struct Hist {
    buckets: [u64; BUCKETS],
    n: u64,
    sum: u128,
    max: u64,
    reservoir: Vec<u32>,
    rng: u64,
}

impl Default for Hist {
    fn default() -> Self {
        Hist {
            buckets: [0; BUCKETS],
            n: 0,
            sum: 0,
            max: 0,
            reservoir: Vec::new(),
            rng: 0x9E37_79B9_7F4A_7C15,
        }
    }
}

impl Hist {
    pub fn record(&mut self, v: u64) {
        self.buckets[bucket(v)] += 1;
        self.n += 1;
        self.sum += v as u128;
        self.max = self.max.max(v);
        let s = v.min(u32::MAX as u64) as u32;
        if self.reservoir.len() < RESERVOIR {
            self.reservoir.push(s);
        } else {
            // Algorithm R with a xorshift generator.
            self.rng ^= self.rng << 13;
            self.rng ^= self.rng >> 7;
            self.rng ^= self.rng << 17;
            let j = self.rng % self.n;
            if (j as usize) < RESERVOIR {
                self.reservoir[j as usize] = s;
            }
        }
    }

    pub fn count(&self) -> u64 {
        self.n
    }

    pub fn max(&self) -> u64 {
        self.max
    }

    /// Nearest-rank percentile (`p` in 0..=100) over the reservoir.
    pub fn percentile(&self, p: f64) -> Option<u64> {
        if self.reservoir.is_empty() {
            return None;
        }
        let mut v = self.reservoir.clone();
        v.sort_unstable();
        let rank = ((p * v.len() as f64 / 100.0) - 1e-9).ceil().max(1.0) as usize;
        Some(v[(rank - 1).min(v.len() - 1)] as u64)
    }

    pub fn to_json(&self) -> Value {
        let mut v = self.reservoir.clone();
        v.sort_unstable();
        let pct = |p: f64| -> Value {
            if v.is_empty() {
                return Value::Null;
            }
            let rank = ((p * v.len() as f64 / 100.0) - 1e-9).ceil().max(1.0) as usize;
            json!(v[(rank - 1).min(v.len() - 1)])
        };
        json!({
            "count": self.n,
            "mean": if self.n > 0 { json!(self.sum as f64 / self.n as f64) } else { Value::Null },
            "max": self.max,
            "p50": pct(50.0),
            "p99": pct(99.0),
            "p99_9": pct(99.9),
            "reservoir": v.len(),
            "log2_buckets": self.buckets.to_vec(),
        })
    }
}

/// Lock-free variant for the device callbacks (buckets and extremes only).
pub struct AtomicHist {
    buckets: [AtomicU64; BUCKETS],
    n: AtomicU64,
    sum: AtomicU64,
    max: AtomicU64,
}

impl Default for AtomicHist {
    fn default() -> Self {
        AtomicHist {
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            n: AtomicU64::new(0),
            sum: AtomicU64::new(0),
            max: AtomicU64::new(0),
        }
    }
}

impl AtomicHist {
    pub fn record(&self, v: u64) {
        self.buckets[bucket(v)].fetch_add(1, Ordering::Relaxed);
        self.n.fetch_add(1, Ordering::Relaxed);
        self.sum.fetch_add(v, Ordering::Relaxed);
        self.max.fetch_max(v, Ordering::Relaxed);
    }

    pub fn to_json(&self) -> Value {
        let n = self.n.load(Ordering::Relaxed);
        json!({
            "count": n,
            "mean": if n > 0 { json!(self.sum.load(Ordering::Relaxed) as f64 / n as f64) } else { Value::Null },
            "max": self.max.load(Ordering::Relaxed),
            "log2_buckets": self.buckets.iter().map(|b| b.load(Ordering::Relaxed)).collect::<Vec<_>>(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_are_exact_below_the_reservoir() {
        let mut h = Hist::default();
        for v in 1..=1000u64 {
            h.record(v);
        }
        assert_eq!(h.percentile(50.0), Some(500));
        assert_eq!(h.percentile(99.0), Some(990));
        assert_eq!(h.percentile(99.9), Some(999));
        assert_eq!(bucket(0), 0);
        assert_eq!(bucket(1), 1);
        assert_eq!(bucket(1023), 10);
    }
}
