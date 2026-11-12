//! The one local clock domain: CLOCK_MONOTONIC in microseconds (docs/room/VOICE.md,
//! "Clocks"). Deliberately not a per-process `Instant` epoch, so values compare with
//! the daemon's `received_at_mono_us` and a tracker's `source_mono_us`.

pub fn mono_us() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid out-pointer; CLOCK_MONOTONIC always exists on Linux.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000 + ts.tv_nsec as u64 / 1000
}

/// Sleep until `target_us` on the mono clock (coarse sleeps, then a short spin-free tail).
pub fn sleep_until(target_us: u64) {
    loop {
        let now = mono_us();
        if now >= target_us {
            return;
        }
        let left = target_us - now;
        std::thread::sleep(std::time::Duration::from_micros(left.min(2_000)));
    }
}
