//! The send path: capture ring -> 20 ms Opus (`VoiceEncoder`) -> `VoiceOut`.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use room_core::rt::RtMsg;
use room_core::voice::{voice_flags, VoiceEnvelope, CODEC_OPUS_48K_MONO};
use voice_core::{VoiceEncoder, VoiceEncoderConfig, VoicePacket};

use crate::clock::mono_us;
use crate::rtio::SendOutcome;
use crate::shared::Shared;

/// The first packets of an incarnation carry `STREAM_START`.
pub const STREAM_START_PACKETS: u64 = 5;
const FRAME_US: u64 = 20_000;

fn new_encoder(sh: &Shared, rate: u32) -> VoiceEncoder {
    VoiceEncoder::new(VoiceEncoderConfig {
        input_sample_rate: rate,
        enable_dtx: sh.cfg.dtx,
        ..Default::default()
    })
    .expect("opus encoder")
}

/// Wire envelope for a `VoicePacket`. The end-of-talkspurt marker has an empty payload
/// and carries `TALKSPURT_END` (CONTRACTS section 6: a 0-length payload is legal only
/// with that flag).
pub fn envelope(p: &VoicePacket, incarnation: u32, sent_count: u64) -> VoiceEnvelope {
    let mut flags = p.flags.bits();
    if sent_count < STREAM_START_PACKETS {
        flags |= voice_flags::STREAM_START;
    }
    VoiceEnvelope {
        codec: CODEC_OPUS_48K_MONO,
        incarnation,
        seq: p.seq,
        timestamp: p.timestamp,
        flags,
        payload: p.payload.clone(),
    }
}

/// How many times to poll the encoder after a pop of `n` capture samples at `rate`.
///
/// `VoiceEncoder::poll_packet` returns `Ok(None)` both for "need more PCM" and for "DTX
/// suppressed this frame", so a loop that stops at the first `None` consumes at most one
/// silent frame per pop and falls behind whenever a pop holds more than one frame. One
/// poll per whole 20 ms frame in the pop (at the capture rate), plus two to work off any
/// remainder.
pub fn poll_budget(n: usize, rate: u32) -> usize {
    let frame = (rate as usize / 50).max(1);
    n / frame + 2
}

/// Poll up to `budget` times, handing every packet to `emit`. A `None` is skipped, not
/// a stop: it may be a suppressed DTX frame with more behind it.
pub fn drain_encoder<F: FnMut(VoicePacket) -> bool>(
    enc: &mut VoiceEncoder,
    budget: usize,
    stats: Option<&crate::shared::Stats>,
    mut emit: F,
) -> bool {
    for _ in 0..budget {
        match enc.poll_packet() {
            Ok(Some(p)) => {
                if !emit(p) {
                    return false;
                }
            }
            Ok(None) => {}
            Err(_) => {
                if let Some(st) = stats {
                    st.bump("encode_errors");
                }
                break;
            }
        }
    }
    true
}

/// Send one packet; `false` when the socket is closed and the loop must end.
fn send_packet(sh: &Shared, p: &VoicePacket, sent: &mut u64) -> bool {
    let env = envelope(p, sh.incarnation, *sent);
    let bytes = env.payload.len() as u64;
    let now = mono_us();
    match sh.conn.send(&RtMsg::VoiceOut {
        sent_mono_us: now,
        env,
    }) {
        SendOutcome::Sent => {
            *sent += 1;
            sh.last_send_us.store(now, Ordering::Relaxed);
            sh.stats.bump("sent_packets");
            sh.stats.add("sent_bytes", bytes);
            true
        }
        SendOutcome::Full => {
            sh.stats.bump("drop_send_full");
            true
        }
        SendOutcome::Closed => {
            sh.stop(crate::shared::ExitReason::SocketClosed);
            false
        }
    }
}

pub fn spawn(sh: Arc<Shared>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("voice-encoder".into())
        .spawn(move || encoder_loop(&sh))
        .expect("spawn encoder")
}

fn encoder_loop(sh: &Shared) {
    // Built on the first samples, once the source's rate is known.
    let mut enc: Option<VoiceEncoder> = None;
    let mut my_gen = sh.gen.device_gen.load(Ordering::SeqCst);
    // After a device change: the packet clock to carry into the replacement encoder.
    let mut pending: Option<((u16, u32), bool)> = None;
    let mut last_frame_end_us = mono_us();
    let mut sent: u64 = 0;
    let mut tmp = vec![0f32; 4096];
    while !sh.is_shutdown() {
        let g = sh.gen.device_gen.load(Ordering::SeqCst);
        if g != my_gen {
            // Device change: drop the half-built frame and every stale mic sample, but
            // keep the packet clock (seq, timestamp) and the incarnation.
            if let Some(mut e) = enc.take() {
                let mut ended = false;
                if let Some(end) = e.finish_talkspurt() {
                    if !send_packet(sh, &end, &mut sent) {
                        return;
                    }
                    ended = true;
                }
                pending = Some((e.packet_clock(), ended));
            }
            sh.cap_ring.clear();
            my_gen = g;
            sh.gen.ack.store(g, Ordering::SeqCst);
            sh.stats.bump("device_changes");
        }
        let n = sh.cap_ring.pop(&mut tmp);
        if n == 0 && enc.is_none() {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        if enc.is_none() {
            let rate = sh.gen.capture_rate.load(Ordering::SeqCst).max(8000);
            let mut e = new_encoder(sh, rate);
            if let Some(((seq, ts), ended)) = pending.take() {
                e.restore_packet_clock(seq, ts);
                // The timestamp counts captured sample time, so the gap while no samples
                // arrived is skipped over in whole frames. An END marker already took the
                // next frame's timestamp (CONTRACTS section 6), so skip at least that one.
                let gap = (mono_us().saturating_sub(last_frame_end_us) + FRAME_US / 2) / FRAME_US;
                e.advance_dropped_frames(if ended { gap.max(1) } else { gap });
            }
            enc = Some(e);
        }
        let rate = sh.gen.capture_rate.load(Ordering::SeqCst).max(8000);
        let enc = enc.as_mut().unwrap();
        if n > 0 {
            enc.push_pcm(&tmp[..n]);
        }
        let ok = drain_encoder(enc, poll_budget(n, rate), Some(&sh.stats), |p| {
            last_frame_end_us = mono_us();
            send_packet(sh, &p, &mut sent)
        });
        if !ok {
            return;
        }
        if n == 0 {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use voice_core::PacketFlags;

    #[test]
    fn envelope_mapping_and_stream_start() {
        let p = VoicePacket {
            seq: 9,
            timestamp: 1920,
            flags: PacketFlags::from_bits(PacketFlags::START_OF_TALKSPURT),
            payload: vec![1, 2],
        };
        let e = envelope(&p, 77, 0);
        assert_eq!(
            e.flags,
            voice_flags::TALKSPURT_START | voice_flags::STREAM_START
        );
        assert_eq!((e.seq, e.timestamp, e.incarnation), (9, 1920, 77));
        let e = envelope(&p, 77, STREAM_START_PACKETS);
        assert_eq!(e.flags, voice_flags::TALKSPURT_START);
        let end = VoicePacket {
            payload: vec![],
            flags: PacketFlags::from_bits(PacketFlags::END_OF_TALKSPURT),
            ..p
        };
        let e = envelope(&end, 77, 9);
        assert!(e.payload.is_empty());
        assert_eq!(e.flags, voice_flags::TALKSPURT_END);
        // The sender's END packet passes the strict decoder.
        assert_eq!(VoiceEnvelope::decode(&e.encode()).unwrap(), e);
    }

    #[test]
    fn dtx_silence_never_builds_an_encoder_backlog() {
        // 60 s of silence in 1024-sample pops (a typical 21 ms cpal period), DTX on.
        let rate = 48_000u32;
        let mut enc = VoiceEncoder::new(VoiceEncoderConfig {
            input_sample_rate: rate,
            enable_dtx: true,
            ..Default::default()
        })
        .unwrap();
        let mut packets = 0;
        let mut pushed: u64 = 0;
        let silence = vec![0f32; 1024];
        let voiced: Vec<f32> = (0..1024)
            .map(|k| 0.3 * (std::f32::consts::TAU * 440.0 * k as f32 / 48_000.0).sin())
            .collect();
        // Speech first, so the encoder has a talkspurt to end.
        for _ in 0..5 {
            enc.push_pcm(&voiced);
            pushed += 1024;
            drain_encoder(&mut enc, poll_budget(1024, rate), None, |_| {
                packets += 1;
                true
            });
        }
        assert!(packets > 0);
        let pops = 60 * 48_000 / 1024;
        for _ in 0..pops {
            enc.push_pcm(&silence);
            pushed += 1024;
            drain_encoder(&mut enc, poll_budget(1024, rate), None, |_| true);
        }
        let lag = pushed - enc.packet_clock().1 as u64;
        assert!(lag < 960, "encoder lag {lag} samples after 60 s of silence");
        // First voiced packet after the silence is stamped with real time (within 40 ms).
        let mut first = None;
        for _ in 0..8 {
            enc.push_pcm(&voiced);
            let before = pushed;
            pushed += 1024;
            drain_encoder(&mut enc, poll_budget(1024, rate), None, |p| {
                if first.is_none() && !p.payload.is_empty() {
                    first = Some((p.timestamp as u64, before));
                }
                true
            });
            if first.is_some() {
                break;
            }
        }
        let (ts, before) = first.expect("voiced packet after silence");
        let err = ts.abs_diff(before);
        assert!(
            err <= 40 * 48,
            "first voiced packet {err} samples from real time"
        );
    }

    #[test]
    fn poll_budget_scales_with_the_pop() {
        assert_eq!(poll_budget(0, 48_000), 2);
        assert_eq!(poll_budget(1024, 48_000), 3);
        assert_eq!(poll_budget(4096, 48_000), 6);
        assert_eq!(poll_budget(882 * 4, 44_100), 6);
    }
}
