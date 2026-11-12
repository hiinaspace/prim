//! A single-producer single-consumer ring of f32 samples built on atomics only, so the
//! device callbacks never lock, allocate or block. Unlike a moved-in `rtrb` half it
//! outlives a stream: after a device change the next stream gets a new handle to the
//! same ring. One producer and one consumer at a time is the caller's contract
//! (`clear` is a consumer operation).

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

pub struct Ring {
    buf: Box<[AtomicU32]>,
    /// Total samples ever written / read; the difference is the fill.
    head: AtomicU64,
    tail: AtomicU64,
}

impl Ring {
    pub fn new(capacity: usize) -> Arc<Ring> {
        Arc::new(Ring {
            buf: (0..capacity).map(|_| AtomicU32::new(0)).collect(),
            head: AtomicU64::new(0),
            tail: AtomicU64::new(0),
        })
    }

    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    pub fn len(&self) -> usize {
        let h = self.head.load(Ordering::Acquire);
        let t = self.tail.load(Ordering::Acquire);
        h.saturating_sub(t) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Producer: write as many samples as fit; returns the number written.
    pub fn push(&self, data: &[f32]) -> usize {
        let h = self.head.load(Ordering::Relaxed);
        let t = self.tail.load(Ordering::Acquire);
        let free = self.buf.len() - (h - t) as usize;
        let n = data.len().min(free);
        for (i, v) in data[..n].iter().enumerate() {
            self.buf[(h as usize + i) % self.buf.len()].store(v.to_bits(), Ordering::Relaxed);
        }
        self.head.store(h + n as u64, Ordering::Release);
        n
    }

    /// Consumer: read up to `out.len()` samples; returns the number read.
    pub fn pop(&self, out: &mut [f32]) -> usize {
        let t = self.tail.load(Ordering::Relaxed);
        let h = self.head.load(Ordering::Acquire);
        let n = out.len().min((h - t) as usize);
        for (i, o) in out[..n].iter_mut().enumerate() {
            *o =
                f32::from_bits(self.buf[(t as usize + i) % self.buf.len()].load(Ordering::Relaxed));
        }
        self.tail.store(t + n as u64, Ordering::Release);
        n
    }

    /// Consumer: discard everything queued.
    pub fn clear(&self) {
        let h = self.head.load(Ordering::Acquire);
        self.tail.store(h, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_and_bounds() {
        let r = Ring::new(8);
        assert_eq!(r.push(&[1.0; 6]), 6);
        assert_eq!(r.push(&[2.0; 6]), 2);
        let mut o = [0.0; 5];
        assert_eq!(r.pop(&mut o), 5);
        assert_eq!(r.push(&[3.0; 4]), 4);
        let mut o = [0.0; 8];
        assert_eq!(r.pop(&mut o), 7);
        assert_eq!(&o[..7], &[1.0, 2.0, 2.0, 3.0, 3.0, 3.0, 3.0]);
        r.push(&[1.0; 3]);
        r.clear();
        assert!(r.is_empty());
    }
}
