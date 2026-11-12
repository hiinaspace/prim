//! Media-time lateness bound for inbound voice (docs/room/VOICE.md, "Queue bounds").
//!
//! The daemon stamps each datagram when it *reads* it. If the daemon is stopped, the
//! kernel (or the transport) keeps the datagrams and they are all read, and stamped,
//! after it resumes: every one looks fresh. The sender's 48 kHz sample timestamp does
//! not lie about that. For each sender we track `offset = arrival - media_time`; its
//! running minimum `base` is the best-case path delay, so `offset - base` is how much
//! later than usual a packet arrived. A packet more than the bound late is dropped.
//!
//! Pure state, no sockets: unit-testable.

/// Allowed slew of `base` upward: the larger of the two clocks' drift (500 ppm each
/// way) with margin, per wall time since the sender's last packet.
const CREEP_PPM: i64 = 1000;
/// A sender with nothing but lateness drops for this long gets `base` reset: a lasting
/// rise in path delay costs this much audio, not minutes.
const ESCAPE_US: u64 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Keep,
    /// Over the lateness bound.
    TooLate,
}

#[derive(Debug, Clone)]
pub struct MediaAge {
    max_late_us: i64,
    incarnation: u32,
    started: bool,
    last_ts: u32,
    unwrapped: i64,
    base: i64,
    last_at_us: u64,
    /// Start of the current run of nothing-but-`TooLate` verdicts.
    drop_run_since: Option<u64>,
}

impl MediaAge {
    pub fn new(max_late_us: u64) -> MediaAge {
        MediaAge {
            max_late_us: max_late_us as i64,
            incarnation: 0,
            started: false,
            last_ts: 0,
            unwrapped: 0,
            base: 0,
            last_at_us: 0,
            drop_run_since: None,
        }
    }

    /// Judge one packet of this sender: `at_us` is the local arrival stamp, `timestamp`
    /// the envelope's 48 kHz sample index (wrapping u32).
    pub fn check(&mut self, at_us: u64, incarnation: u32, timestamp: u32) -> Verdict {
        if !self.started || incarnation != self.incarnation {
            self.incarnation = incarnation;
            self.started = false;
            self.drop_run_since = None;
        }
        // Unwrap by the signed distance from the newest timestamp seen (late packets of
        // a reordered pair go backwards a little, which is fine).
        if self.started {
            let d = timestamp.wrapping_sub(self.last_ts) as i32 as i64;
            self.unwrapped += d;
        } else {
            self.unwrapped = timestamp as i64;
        }
        self.last_ts = timestamp;
        let ts_us = self.unwrapped * 125 / 6; // samples * 1e6 / 48000
        let offset = at_us as i64 - ts_us;
        if !self.started {
            self.started = true;
            self.base = offset;
            self.last_at_us = at_us;
            return Verdict::Keep;
        }
        let dt = at_us.saturating_sub(self.last_at_us) as i64;
        self.last_at_us = at_us;
        self.base = offset.min(self.base + dt * CREEP_PPM / 1_000_000);
        if offset - self.base <= self.max_late_us {
            self.drop_run_since = None;
            return Verdict::Keep;
        }
        let since = *self.drop_run_since.get_or_insert(at_us);
        if at_us.saturating_sub(since) > ESCAPE_US {
            self.base = offset;
            self.drop_run_since = None;
            return Verdict::Keep;
        }
        Verdict::TooLate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX: u64 = 100_000;
    const FRAME: u64 = 20_000;

    fn ts_of(us: u64) -> u32 {
        (us * 48 / 1000) as u32
    }

    #[test]
    fn drift_within_500_ppm_either_way_never_drops() {
        for ppm in [-500i64, 500] {
            let mut m = MediaAge::new(MAX);
            let mut drops = 0;
            // 60 s; the sender's media clock runs `ppm` fast relative to ours.
            for i in 0..3000u64 {
                let media_us = i * FRAME;
                let at = 5_000_000
                    + (media_us as i64 * (1_000_000 - ppm) / 1_000_000) as u64
                    + (i % 7) * 400; // a little jitter
                if m.check(at, 1, ts_of(media_us)) == Verdict::TooLate {
                    drops += 1;
                }
            }
            assert_eq!(drops, 0, "ppm {ppm}");
        }
    }

    #[test]
    fn burst_after_a_pause_drops_old_keeps_new() {
        let mut m = MediaAge::new(MAX);
        let mut t = 1_000_000u64;
        let mut i = 0u64;
        for _ in 0..50 {
            assert_eq!(m.check(t, 1, ts_of(i * FRAME)), Verdict::Keep);
            i += 1;
            t += FRAME;
        }
        // Reader stopped 500 ms; 25 packets queued up, all stamped at resume.
        let resume = t + 500_000;
        let mut kept = Vec::new();
        for k in 0..25u64 {
            let at = resume + k * 100; // read in a burst
            if m.check(at, 1, ts_of((i + k) * FRAME)) == Verdict::Keep {
                kept.push(k);
            }
        }
        i += 25;
        // Packet k's media time is k*20 ms after the pause began; lateness at resume
        // is 500 - k*20 ms, so those within 100 ms of fresh survive.
        assert!(kept.iter().all(|&k| k >= 20), "old ones dropped: {kept:?}");
        assert!(
            kept.contains(&24) && kept.contains(&21),
            "new kept: {kept:?}"
        );
        // Back to real time: everything kept.
        let mut at = resume + 2_500;
        for k in 0..50u64 {
            at += FRAME;
            assert_eq!(
                m.check(at, 1, ts_of((i + k) * FRAME)),
                Verdict::Keep,
                "k {k}"
            );
        }
    }

    #[test]
    fn permanent_step_recovers_within_about_a_second() {
        let mut m = MediaAge::new(MAX);
        let mut t = 1_000_000u64;
        let mut i = 0u64;
        for _ in 0..50 {
            m.check(t, 1, ts_of(i * FRAME));
            i += 1;
            t += FRAME;
        }
        t += 200_000; // path delay +200 ms for good
        let start = t;
        let mut first_kept = None;
        for _ in 0..200 {
            if m.check(t, 1, ts_of(i * FRAME)) == Verdict::Keep && first_kept.is_none() {
                first_kept = Some(t - start);
            }
            i += 1;
            t += FRAME;
        }
        let f = first_kept.expect("recovers");
        assert!((900_000..=1_100_000).contains(&f), "recovered after {f} us");
        // and stays recovered
        assert_eq!(m.check(t, 1, ts_of(i * FRAME)), Verdict::Keep);
    }

    #[test]
    fn timestamp_wrap() {
        let mut m = MediaAge::new(MAX);
        let mut t = 1_000_000u64;
        let start = u32::MAX - 48 * 20 * 10; // ten frames before the wrap
        for k in 0..100u32 {
            let ts = start.wrapping_add(k * 960);
            assert_eq!(m.check(t, 1, ts), Verdict::Keep, "k {k}");
            t += FRAME;
        }
    }

    #[test]
    fn new_incarnation_resets() {
        let mut m = MediaAge::new(MAX);
        let mut t = 1_000_000u64;
        for k in 0..50u64 {
            m.check(t, 1, ts_of(k * FRAME));
            t += FRAME;
        }
        // A restart: unrelated timestamp origin, long gap. Must not be judged against
        // the old stream.
        t += 3_000_000;
        for k in 0..50u64 {
            assert_eq!(m.check(t, 2, ts_of(900_000_000 + k * FRAME)), Verdict::Keep);
            t += FRAME;
        }
    }

    #[test]
    fn silence_that_advances_in_real_time_is_not_late() {
        // DTX / talkspurt gaps: no packets for 5 s, but media time moved with the clock.
        let mut m = MediaAge::new(MAX);
        let mut t = 1_000_000u64;
        for k in 0..50u64 {
            m.check(t, 1, ts_of(k * FRAME));
            t += FRAME;
        }
        let gap = 5_000_000;
        assert_eq!(m.check(t + gap, 1, ts_of(50 * FRAME + gap)), Verdict::Keep);
    }
}
