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

/// Wire envelope for a `VoicePacket`; `None` for the empty end-of-talkspurt marker,
/// which the frozen envelope can't carry (payload must be 1..=1000 bytes).
pub fn envelope(p: &VoicePacket, incarnation: u32, sent_count: u64) -> Option<VoiceEnvelope> {
    if p.payload.is_empty() {
        return None;
    }
    let mut flags = p.flags.bits();
    if sent_count < STREAM_START_PACKETS {
        flags |= voice_flags::STREAM_START;
    }
    Some(VoiceEnvelope {
        codec: CODEC_OPUS_48K_MONO,
        incarnation,
        seq: p.seq,
        timestamp: p.timestamp,
        flags,
        payload: p.payload.clone(),
    })
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
    let mut pending: Option<(u16, u32)> = None;
    let mut last_frame_end_us = mono_us();
    let mut sent: u64 = 0;
    // Empty end-of-talkspurt markers can't ride the envelope (payload 1..=1000), but
    // they consume an encoder sequence number; subtracting the count keeps the wire's
    // seq at +1 per packet sent (CONTRACTS section 6).
    let mut unsent_markers: u16 = 0;
    let mut tmp = vec![0f32; 4096];
    while !sh.is_shutdown() {
        let g = sh.gen.device_gen.load(Ordering::SeqCst);
        if g != my_gen {
            // Device change: drop the half-built frame and every stale mic sample, but
            // keep the packet clock (seq, timestamp) and the incarnation.
            if let Some(mut e) = enc.take() {
                if e.finish_talkspurt().is_some() {
                    unsent_markers = unsent_markers.wrapping_add(1);
                    sh.stats.bump("end_markers_not_sent");
                }
                pending = Some(e.packet_clock());
            }
            sh.cap_ring.clear();
            my_gen = g;
            sh.gen.ack.store(g, Ordering::SeqCst);
            sh.stats.bump("device_changes");
        }
        let n = sh.cap_ring.pop(&mut tmp);
        if n == 0 {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        if enc.is_none() {
            let rate = sh.gen.capture_rate.load(Ordering::SeqCst).max(8000);
            let mut e = new_encoder(sh, rate);
            if let Some((seq, ts)) = pending.take() {
                e.restore_packet_clock(seq, ts);
                // The timestamp counts captured sample time, so the gap while no samples
                // arrived is skipped over in whole frames.
                let gap = (mono_us().saturating_sub(last_frame_end_us) + FRAME_US / 2) / FRAME_US;
                e.advance_dropped_frames(gap);
            }
            enc = Some(e);
        }
        let enc = enc.as_mut().unwrap();
        enc.push_pcm(&tmp[..n]);
        loop {
            match enc.poll_packet() {
                Ok(Some(p)) => {
                    last_frame_end_us = mono_us();
                    let Some(mut env) = envelope(&p, sh.incarnation, sent) else {
                        unsent_markers = unsent_markers.wrapping_add(1);
                        sh.stats.bump("end_markers_not_sent");
                        continue;
                    };
                    env.seq = env.seq.wrapping_sub(unsent_markers);
                    let bytes = env.payload.len() as u64;
                    let now = mono_us();
                    match sh.conn.send(&RtMsg::VoiceOut {
                        sent_mono_us: now,
                        env,
                    }) {
                        SendOutcome::Sent => {
                            sent += 1;
                            sh.last_send_us.store(now, Ordering::Relaxed);
                            sh.stats.bump("sent_packets");
                            sh.stats.add("sent_bytes", bytes);
                        }
                        SendOutcome::Full => sh.stats.bump("drop_send_full"),
                        SendOutcome::Closed => {
                            sh.stop(crate::shared::ExitReason::SocketClosed);
                            return;
                        }
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    sh.stats.bump("encode_errors");
                    break;
                }
            }
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
        let e = envelope(&p, 77, 0).unwrap();
        assert_eq!(
            e.flags,
            voice_flags::TALKSPURT_START | voice_flags::STREAM_START
        );
        assert_eq!((e.seq, e.timestamp, e.incarnation), (9, 1920, 77));
        let e = envelope(&p, 77, STREAM_START_PACKETS).unwrap();
        assert_eq!(e.flags, voice_flags::TALKSPURT_START);
        let end = VoicePacket {
            payload: vec![],
            flags: PacketFlags::from_bits(PacketFlags::END_OF_TALKSPURT),
            ..p
        };
        assert!(
            envelope(&end, 77, 9).is_none(),
            "frozen envelope can't carry an empty payload"
        );
    }
}
