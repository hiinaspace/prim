//! Small pure pieces of the daemon's inbound lane (docs/room/VOICE.md, "Queue bounds"
//! and "Rate limits"): the stall detector that gates media-time drops, and the token
//! bucket behind the per-sender rate limits. No clocks, no sockets: callers pass `now`.

/// A gap between two looks at the clock longer than this is a stall of the lane.
pub const STALL_GAP_US: u64 = 50_000;
/// How long after a stall the media-time bound may still drop, on top of the stall's
/// own length.
pub const WINDOW_MARGIN_US: u64 = 100_000;
/// The stall length that counts toward the window, at most.
pub const WINDOW_STALL_CAP_US: u64 = 1_000_000;

/// Detects that the lane (the daemon's realtime thread) was itself stopped: a SIGSTOP,
/// a blocked executor, a suspended VM.
///
/// The lane calls [`observe`](StallGate::observe) at every event, and a 10 ms timer
/// guarantees an event when idle, so the gap between observations is the longest the
/// thread went without looking. Observing at the head of `on_ingress` as well means a
/// backlog packet is judged by a gate that has already seen the stall, whichever event
/// the thread handled first after resuming.
///
/// The window in which dropping is allowed is `now + margin + min(stall, cap)` from the
/// end of the stall: the backlog is read within milliseconds of resuming, but a
/// transport that releases it in steps (QUIC catching up after a stop) can take about
/// as long as the stall was long, so the window scales with it. It is time-based
/// because "the backlog has drained" is not observable: new packets of a sender look
/// like backlog until the sender's own media-time offset says otherwise.
#[derive(Debug, Default, Clone)]
pub struct StallGate {
    last_us: u64,
    window_until_us: u64,
    /// Stalls detected so far.
    pub stalls: u64,
}

impl StallGate {
    /// Look at the clock. Returns whether `now` is inside a post-stall window.
    pub fn observe(&mut self, now: u64) -> bool {
        if self.last_us != 0 {
            let gap = now.saturating_sub(self.last_us);
            if gap > STALL_GAP_US {
                self.stalls += 1;
                let until = now + WINDOW_MARGIN_US + gap.min(WINDOW_STALL_CAP_US);
                self.window_until_us = self.window_until_us.max(until);
            }
        }
        self.last_us = now;
        now < self.window_until_us
    }
}

/// Token bucket in integer micro-tokens: `rate_pps` tokens per second up to `burst`.
#[derive(Debug, Clone)]
pub struct Bucket {
    /// Micro-tokens (1 token = 1_000_000).
    tokens: u64,
    last_us: u64,
    rate_pps: u64,
    cap: u64,
}

impl Bucket {
    pub fn new(rate_pps: u64, burst: u64) -> Bucket {
        Bucket {
            tokens: burst * 1_000_000,
            last_us: 0,
            rate_pps,
            cap: burst * 1_000_000,
        }
    }

    /// Take one token at `now_us` (any monotonic stamp; going backwards adds nothing).
    pub fn take(&mut self, now_us: u64) -> bool {
        if self.last_us != 0 {
            let dt = now_us.saturating_sub(self.last_us);
            self.tokens = (self.tokens + dt * self.rate_pps).min(self.cap);
        }
        self.last_us = self.last_us.max(now_us);
        if self.tokens >= 1_000_000 {
            self.tokens -= 1_000_000;
            true
        } else {
            false
        }
    }
}

/// Voice datagrams per sender: 50 pps nominal (one per 20 ms frame). Visemes (kind 3)
/// are decoded and dropped in R0, so they need no bucket.
pub const DGRAM_RATE_PPS: u64 = 100;
pub const DGRAM_BURST: u64 = 50;
/// New streams (incarnations) per sender: one per second after a burst of three.
pub const STREAM_RATE_PPS: u64 = 1;
pub const STREAM_BURST: u64 = 3;

/// All of one sender's buckets.
#[derive(Debug, Clone)]
pub struct SenderLimits {
    pub voice: Bucket,
    pub streams: Bucket,
}

impl Default for SenderLimits {
    fn default() -> Self {
        SenderLimits {
            voice: Bucket::new(DGRAM_RATE_PPS, DGRAM_BURST),
            streams: Bucket::new(STREAM_RATE_PPS, STREAM_BURST),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Look at the clock every 10 ms from `from` to `to`; the last answer.
    fn run(g: &mut StallGate, from: u64, to: u64) -> bool {
        let mut t = from;
        let mut w = false;
        while t <= to {
            w = g.observe(t);
            t += 10_000;
        }
        w
    }

    #[test]
    fn gate_sees_a_gap_and_closes_after_the_window() {
        let mut g = StallGate::default();
        assert!(!run(&mut g, 1_000_000, 2_000_000));
        assert_eq!(g.stalls, 0);
        let resume = 2_500_000; // 500 ms between looks
        assert!(g.observe(resume));
        assert_eq!(g.stalls, 1);
        // The window is the margin plus the stall length from the resume.
        let end = resume + WINDOW_MARGIN_US + 500_000;
        assert!(run(&mut g, resume + 10_000, end - 10_000));
        assert!(!g.observe(end + 1));
        // 40 ms of scheduler slop is not a stall.
        assert!(!g.observe(end + 41_000));
        assert_eq!(g.stalls, 1);
    }

    #[test]
    fn gate_window_is_capped_and_extends() {
        let mut g = StallGate::default();
        g.observe(1_000_000);
        let t = 1_000_000 + 60_000_000; // a minute stopped
        assert!(g.observe(t));
        let end = t + WINDOW_MARGIN_US + WINDOW_STALL_CAP_US;
        assert!(run(&mut g, t + 10_000, end - 10_000));
        assert!(!g.observe(end + 1));
        // A second stall inside an open window extends it.
        let mut g = StallGate::default();
        g.observe(1_000_000);
        let t1 = 1_200_000;
        assert!(g.observe(t1)); // until t1 + 300 ms
        let t2 = t1 + 200_000;
        assert!(g.observe(t2)); // second stall: until t2 + 300 ms
        assert!(run(&mut g, t2 + 10_000, t2 + 290_000));
    }

    #[test]
    fn first_look_is_not_a_stall() {
        let mut g = StallGate::default();
        assert!(!g.observe(9_000_000_000));
        assert_eq!(g.stalls, 0);
    }

    #[test]
    fn bucket_allows_burst_then_the_rate() {
        let mut b = Bucket::new(100, 50);
        let t0 = 1_000_000;
        let ok = (0..80).filter(|_| b.take(t0)).count();
        assert_eq!(ok, 50, "burst");
        // 100 pps: 10 ms is one token.
        assert!(!b.take(t0 + 5_000));
        assert!(b.take(t0 + 10_000));
        assert!(!b.take(t0 + 10_000));
        // After a long idle it is full again, and no fuller.
        let ok = (0..80).filter(|_| b.take(t0 + 60_000_000)).count();
        assert_eq!(ok, 50);
    }

    #[test]
    fn bucket_passes_nominal_voice_forever() {
        let mut b = Bucket::new(DGRAM_RATE_PPS, DGRAM_BURST);
        let mut t = 5_000_000;
        for i in 0..50_000 {
            // 50 pps with 5 ms of jitter either way
            let at = t + (i % 3) * 5_000;
            assert!(b.take(at), "packet {i}");
            t += 20_000;
        }
    }

    #[test]
    fn bucket_clock_going_backwards_adds_nothing() {
        let mut b = Bucket::new(1, 3);
        let t = 10_000_000;
        assert!(b.take(t) && b.take(t) && b.take(t));
        assert!(!b.take(t));
        assert!(!b.take(t - 5_000_000));
        assert!(!b.take(t + 500_000));
        assert!(b.take(t + 1_000_000));
    }

    #[test]
    fn stream_bucket_is_one_per_second_after_three() {
        let mut b = Bucket::new(STREAM_RATE_PPS, STREAM_BURST);
        let t = 1_000_000;
        assert_eq!((0..10).filter(|_| b.take(t)).count(), 3);
        assert!(b.take(t + 1_000_000));
        assert!(!b.take(t + 1_500_000));
    }
}
