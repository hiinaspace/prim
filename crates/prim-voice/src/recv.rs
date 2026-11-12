//! The rt reader: decodes daemon messages, fences voice by incarnation per sender and
//! feeds each sender's NetEq with the daemon's arrival stamp.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use room_core::ids::Key;
use room_core::rt::{peer_flags, RtMsg};
use room_core::voice::{voice_flags, Fence, VoiceEnvelope};
use voice_core::{PacketArrival, PacketFlags, VoicePacket, VoiceReceiver};

use crate::clock::mono_us;
use crate::config::ArrivalPolicy;
use crate::shared::{ExitReason, ListenerState, PeerRx, Shared};
use crate::tone;

/// A `VoiceIn` older than this at read is dropped (VOICE.md, queue bounds).
pub const MAX_AGE_US: u64 = 100_000;
/// A stamp more than this old means the clock domains don't match.
pub const MISMATCH_AGE_US: u64 = 1_000_000;
pub const KEEPALIVE_US: u64 = 250_000;

pub fn spawn(sh: Arc<Shared>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("voice-rt-reader".into())
        .spawn(move || reader_loop(&sh))
        .expect("spawn reader")
}

fn reader_loop(sh: &Shared) {
    while !sh.is_shutdown() {
        match sh.conn.recv(Duration::from_millis(20)) {
            Ok(Some(b)) => {
                let read_us = mono_us();
                match RtMsg::decode(&b) {
                    Ok(m) => handle(sh, m, read_us),
                    Err(_) => sh.stats.bump("drop_rt_decode"),
                }
            }
            Ok(None) => {}
            Err(_) => {
                sh.stop(ExitReason::SocketClosed);
                return;
            }
        }
        // Keepalive when idle: any message renews the lease, so only send if we haven't.
        let now = mono_us();
        if now.saturating_sub(sh.last_send_us.load(Ordering::Relaxed)) >= KEEPALIVE_US {
            if sh.conn.send(&RtMsg::Keepalive) == crate::rtio::SendOutcome::Closed {
                sh.stop(ExitReason::SocketClosed);
                return;
            }
            sh.last_send_us.store(now, Ordering::Relaxed);
            sh.stats.bump("keepalives");
        }
    }
}

fn handle(sh: &Shared, m: RtMsg, read_us: u64) {
    match m {
        RtMsg::VoiceIn {
            from,
            received_at_mono_us,
            env,
        } => voice_in(sh, from, received_at_mono_us, env, read_us),
        RtMsg::Listener {
            source_mono_us,
            forwarded_mono_us,
            seq,
            pose,
        } => {
            *sh.listener.lock().unwrap() = Some(ListenerState {
                source_mono_us,
                forwarded_mono_us,
                seq,
                pose,
            });
        }
        RtMsg::Peer { key, flags, .. } => {
            if key != sh.self_key && flags & peer_flags::CONNECTED != 0 {
                sh.seats.lock().unwrap().add(key);
            }
        }
        RtMsg::Revoked { reason } => sh.stop(ExitReason::Revoked(reason)),
        _ => {}
    }
}

/// What to do with one `VoiceIn`, decided from its stamps alone.
#[derive(Debug, PartialEq, Eq)]
pub enum Timing {
    /// Use this arrival time; `delay_us` is the IPC delay to record (None if the
    /// stamp was unusable and the read time stands in for it).
    Accept {
        arrival_us: u64,
        delay_us: Option<u64>,
    },
    /// Over 100 ms old at read.
    TooOld,
}

pub fn judge_timing(
    received_at_us: u64,
    read_us: u64,
    policy: ArrivalPolicy,
    mismatch: &mut bool,
) -> Timing {
    *mismatch = false;
    // A stamp in the future or over 1 s old: the domains don't match, use read time.
    if received_at_us > read_us || read_us - received_at_us > MISMATCH_AGE_US {
        *mismatch = true;
        return Timing::Accept {
            arrival_us: read_us,
            delay_us: None,
        };
    }
    let delay = read_us - received_at_us;
    if delay > MAX_AGE_US {
        return Timing::TooOld;
    }
    Timing::Accept {
        arrival_us: match policy {
            ArrivalPolicy::Stamp => received_at_us,
            ArrivalPolicy::ReadTime => read_us,
        },
        delay_us: Some(delay),
    }
}

fn new_receiver(sh: &Shared) -> voice_core::Result<VoiceReceiver> {
    match sh.cfg.min_delay_ms {
        None => VoiceReceiver::new(crate::shared::SAMPLE_RATE),
        Some(min) => {
            VoiceReceiver::new_with_delay_bounds(crate::shared::SAMPLE_RATE, min, 250.max(min))
        }
    }
}

fn voice_in(sh: &Shared, from: Key, received_at_us: u64, env: VoiceEnvelope, read_us: u64) {
    sh.stats.bump("voice_in");
    let mut mismatch = false;
    let (arrival_us, delay_us) =
        match judge_timing(received_at_us, read_us, sh.cfg.arrival, &mut mismatch) {
            Timing::Accept {
                arrival_us,
                delay_us,
            } => (arrival_us, delay_us),
            Timing::TooOld => {
                sh.stats.bump("drop_age");
                return;
            }
        };
    if mismatch {
        sh.stats.bump("clock_mismatch");
    }
    if let Some(d) = delay_us {
        sh.stats.inner.lock().unwrap().ipc_delay_us.record(d);
    }
    sh.seats.lock().unwrap().add(from);

    let peer_arc = sh
        .senders
        .lock()
        .unwrap()
        .map
        .entry(from)
        .or_insert_with(|| Arc::new(Mutex::new(PeerRx::new())))
        .clone();
    let mut peer = peer_arc.lock().unwrap();
    match peer.fence.judge(env.incarnation) {
        Fence::Stale => {
            peer.stale += 1;
            drop(peer);
            sh.stats.bump("fence_stale");
            return;
        }
        Fence::NewStream { .. } => {
            peer.new_streams += 1;
            peer.incarnation = env.incarnation;
            peer.needs_reset = true;
            match peer.receiver.as_mut() {
                Some(r) => {
                    let _ = r.reset_stream();
                }
                None => match new_receiver(sh) {
                    Ok(r) => peer.receiver = Some(r),
                    Err(_) => {
                        peer.push_errors += 1;
                        drop(peer);
                        sh.stats.bump("drop_push_error");
                        return;
                    }
                },
            }
            sh.stats.bump("fence_new_stream");
            let hz = tone::auto_hz(env.incarnation);
            let mut tones = sh.tones.lock().unwrap();
            if !tones.iter().any(|(i, _)| *i == env.incarnation) {
                tones.push((env.incarnation, hz));
            }
        }
        Fence::Current => {}
    }
    let Some(rx) = peer.receiver.as_mut() else {
        return;
    };
    let pkt = VoicePacket {
        seq: env.seq,
        timestamp: env.timestamp,
        // The wire's low three flag bits are voice-core's; STREAM_START is ours alone.
        flags: PacketFlags::from_bits(
            env.flags & (voice_flags::KNOWN & !voice_flags::STREAM_START),
        ),
        payload: env.payload,
    };
    // `now` is read just before the call: voice-core turns (arrival, now) into the
    // Instant NetEq's delay estimator sees, `Instant::now() - (now - arrival)`.
    let now = mono_us();
    match rx.push_packet_with_now_mono(
        pkt,
        PacketArrival {
            received_at_mono_us: arrival_us,
        },
        now,
    ) {
        Ok(()) => {
            peer.packets_accepted += 1;
            peer.last_packet_us = now;
            peer.active = true;
        }
        Err(_) => {
            peer.push_errors += 1;
            drop(peer);
            sh.stats.bump("drop_push_error");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timing_rules() {
        let mut mm = false;
        let t = judge_timing(1_000_000, 1_003_000, ArrivalPolicy::Stamp, &mut mm);
        assert_eq!(
            t,
            Timing::Accept {
                arrival_us: 1_000_000,
                delay_us: Some(3_000)
            }
        );
        assert!(!mm);
        let t = judge_timing(1_000_000, 1_003_000, ArrivalPolicy::ReadTime, &mut mm);
        assert_eq!(
            t,
            Timing::Accept {
                arrival_us: 1_003_000,
                delay_us: Some(3_000)
            }
        );
        // over 100 ms old: dropped
        assert_eq!(
            judge_timing(1_000_000, 1_150_000, ArrivalPolicy::Stamp, &mut mm),
            Timing::TooOld
        );
        // future stamp, and over 1 s old: read time stands in, counted
        let t = judge_timing(9_000_000, 1_000_000, ArrivalPolicy::Stamp, &mut mm);
        assert_eq!(
            t,
            Timing::Accept {
                arrival_us: 1_000_000,
                delay_us: None
            }
        );
        assert!(mm);
        let t = judge_timing(10, 2_000_000, ArrivalPolicy::Stamp, &mut mm);
        assert_eq!(
            t,
            Timing::Accept {
                arrival_us: 2_000_000,
                delay_us: None
            }
        );
        assert!(mm);
    }
}
