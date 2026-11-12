//! Voice envelope, visemes, the `member/<id>/voice` value and the incarnation fence
//! (CONTRACTS.md section 6). Datagram kinds 2 and 3 carry the first two.
//!
//! The envelope has no sender field: the sender is the authenticated remote key of the
//! connection it arrived on, which the hello bound to the member and the room.
//! Datagrams are never forwarded, so that binding is exact.

use crate::wire::{DecodeError, Reader, Writer};

/// Envelope version (high nibble of `voice_ver`).
pub const VOICE_VERSION: u8 = 1;
/// Codec 1: Opus, 48 kHz, mono (low nibble of `voice_ver`).
pub const CODEC_OPUS_48K_MONO: u8 = 1;
/// Largest Opus payload carried. 1 + 14 + 1000 bytes stays under the ~1.2 KB floor.
pub const MAX_VOICE_PAYLOAD: usize = 1000;
pub const VOICE_HEADER_LEN: usize = 14;
/// Visemes carried per datagram, `count` 0..=32. Weights are indexed in `VISEME_ORDER`;
/// a receiver ignores indices >= 15 (`VISEME_COUNT`) and treats `count = 0` as neutral.
pub const MAX_VISEMES: usize = 32;
/// The 15 visemes, in weight-index order.
pub const VISEME_ORDER: [&str; VISEME_COUNT] = [
    "sil", "PP", "FF", "TH", "DD", "kk", "CH", "SS", "nn", "RR", "aa", "E", "ih", "oh", "ou",
];
pub const VISEME_COUNT: usize = 15;
/// Retired incarnations remembered per sender.
pub const FENCE_RETIRED: usize = 8;

pub mod voice_flags {
    pub const TALKSPURT_START: u8 = 1 << 0;
    pub const TALKSPURT_END: u8 = 1 << 1;
    pub const FEC: u8 = 1 << 2;
    /// Set on the first packets of an incarnation (the sender repeats it on the first
    /// five), so a receiver can tell a fresh stream from one it joined mid-way.
    pub const STREAM_START: u8 = 1 << 3;
    pub const KNOWN: u8 = TALKSPURT_START | TALKSPURT_END | FEC | STREAM_START;
}

/// One voice packet: the body of datagram kind 2 (everything after `ver_kind`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceEnvelope {
    pub codec: u8,
    /// Random, nonzero, fresh per stream start; never reused by a sender.
    pub incarnation: u32,
    /// +1 per packet, wraps.
    pub seq: u16,
    /// 48 kHz sample index of the packet's first sample, wraps. A packet carries exactly
    /// one 20 ms Opus frame and the timestamp advances by 960 per frame, counting frames
    /// the sender suppressed (DTX), so silence keeps pace with real time.
    pub timestamp: u32,
    pub flags: u8,
    /// One 20 ms Opus frame, 1..=1000 bytes; empty only with `TALKSPURT_END` (the end
    /// marker, whose `timestamp` is the next frame's).
    pub payload: Vec<u8>,
}

impl VoiceEnvelope {
    pub fn encode_into(&self, w: &mut Writer) {
        w.u8((VOICE_VERSION << 4) | (self.codec & 0x0f))
            .u32(self.incarnation)
            .u16(self.seq)
            .u32(self.timestamp)
            .u8(self.flags)
            .bytes16(&self.payload);
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        self.encode_into(&mut w);
        w.finish()
    }

    pub fn decode_from(r: &mut Reader) -> Result<VoiceEnvelope, DecodeError> {
        let vv = r.u8()?;
        if vv >> 4 != VOICE_VERSION {
            return Err(DecodeError::Version(vv >> 4));
        }
        let codec = vv & 0x0f;
        if codec != CODEC_OPUS_48K_MONO {
            return Err(DecodeError::Kind(codec));
        }
        let incarnation = r.u32()?;
        if incarnation == 0 {
            return Err(DecodeError::Field("incarnation 0"));
        }
        let seq = r.u16()?;
        let timestamp = r.u32()?;
        let flags = r.u8()?;
        if flags & !voice_flags::KNOWN != 0 {
            return Err(DecodeError::Field("voice flags"));
        }
        let payload = r.bytes16(MAX_VOICE_PAYLOAD, "voice payload")?;
        // A 0-length payload is the end-of-talkspurt marker: legal only with that flag.
        if payload.is_empty() && flags & voice_flags::TALKSPURT_END == 0 {
            return Err(DecodeError::Field("empty voice payload"));
        }
        Ok(VoiceEnvelope {
            codec,
            incarnation,
            seq,
            timestamp,
            flags,
            payload: payload.to_vec(),
        })
    }

    pub fn decode(b: &[u8]) -> Result<VoiceEnvelope, DecodeError> {
        let mut r = Reader::new(b);
        let e = Self::decode_from(&mut r)?;
        r.end()?;
        Ok(e)
    }
}

/// Mouth shapes for one analysis window: the body of datagram kind 3.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Viseme {
    /// The voice stream incarnation these visemes describe.
    pub incarnation: u32,
    /// 48 kHz timestamp (the voice packet clock) at the centre of the analysis window.
    pub sample_pos: u32,
    pub weights: Vec<u8>,
}

pub const VISEME_VERSION: u8 = 1;

impl Viseme {
    pub fn encode_into(&self, w: &mut Writer) {
        w.u8(VISEME_VERSION)
            .u32(self.incarnation)
            .u32(self.sample_pos)
            .bytes8(&self.weights);
    }

    pub fn decode_from(r: &mut Reader) -> Result<Viseme, DecodeError> {
        let v = r.u8()?;
        if v != VISEME_VERSION {
            return Err(DecodeError::Version(v));
        }
        let incarnation = r.u32()?;
        if incarnation == 0 {
            return Err(DecodeError::Field("incarnation 0"));
        }
        let sample_pos = r.u32()?;
        let weights = r.bytes8(MAX_VISEMES, "visemes")?.to_vec();
        Ok(Viseme {
            incarnation,
            sample_pos,
            weights,
        })
    }
}

/// Name of the member key the daemon writes from voice-lease presence.
pub const VOICE_KEY: &str = "voice";

pub mod voice_state {
    /// A voice client holds the lease and is rendering received voice.
    pub const LISTENING: u8 = 1 << 0;
    /// The client is capturing and sending (not muted).
    pub const MIC: u8 = 1 << 1;
}

/// Value of `member/<id>/voice` (durable, presence-bound). Written only by the daemon,
/// from the voice lease: `ver u8 = 1, flags u8, incarnation u32` (0 = not sending).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VoiceState {
    pub flags: u8,
    pub incarnation: u32,
}

impl VoiceState {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u8(1).u8(self.flags).u32(self.incarnation);
        w.finish()
    }

    pub fn decode(b: &[u8]) -> Result<VoiceState, DecodeError> {
        let mut r = Reader::new(b);
        let v = r.u8()?;
        if v != 1 {
            return Err(DecodeError::Version(v));
        }
        let flags = r.u8()?;
        if flags & !(voice_state::LISTENING | voice_state::MIC) != 0 {
            return Err(DecodeError::Field("voice state flags"));
        }
        let incarnation = r.u32()?;
        r.end()?;
        Ok(VoiceState { flags, incarnation })
    }
}

/// What a receiver does with a packet, judged by its incarnation alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fence {
    /// Same stream as before.
    Current,
    /// A new stream: reset jitter-buffer and decoder state, then accept.
    /// `previous` is the incarnation that was just retired (0 if none).
    NewStream { previous: u32 },
    /// A retired incarnation: drop it.
    Stale,
}

/// Per-sender incarnation fence. Incarnations are random, so "newer" can't be
/// compared; instead anything not current and not retired is a new stream, and a
/// retired one is dropped forever (up to `FENCE_RETIRED` of them).
#[derive(Clone, Debug, Default)]
pub struct IncarnationFence {
    current: u32,
    retired: [u32; FENCE_RETIRED],
    next: usize,
}

impl IncarnationFence {
    pub fn current(&self) -> u32 {
        self.current
    }

    pub fn judge(&mut self, incarnation: u32) -> Fence {
        if incarnation == 0 || self.retired.contains(&incarnation) {
            return Fence::Stale;
        }
        if incarnation == self.current {
            return Fence::Current;
        }
        let previous = self.current;
        if previous != 0 {
            self.retire(previous);
        }
        self.current = incarnation;
        Fence::NewStream { previous }
    }

    /// Retire the current stream without a successor (the sender said it stopped,
    /// or its `member/<id>/voice` incarnation changed).
    pub fn retire_current(&mut self) {
        if self.current != 0 {
            let c = self.current;
            self.retire(c);
            self.current = 0;
        }
    }

    fn retire(&mut self, inc: u32) {
        self.retired[self.next] = inc;
        self.next = (self.next + 1) % FENCE_RETIRED;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(inc: u32) -> VoiceEnvelope {
        VoiceEnvelope {
            codec: CODEC_OPUS_48K_MONO,
            incarnation: inc,
            seq: 7,
            timestamp: 960,
            flags: voice_flags::STREAM_START,
            payload: vec![0xfc, 1, 2],
        }
    }

    #[test]
    fn envelope_round_trips_and_is_strict() {
        let e = env(0xdead_beef);
        let b = e.encode();
        assert_eq!(b.len(), VOICE_HEADER_LEN + 3);
        assert_eq!(VoiceEnvelope::decode(&b).unwrap(), e);
        let mut t = b.clone();
        t.push(0);
        assert!(VoiceEnvelope::decode(&t).is_err(), "trailing byte");
        assert!(
            VoiceEnvelope::decode(&env(0).encode()).is_err(),
            "incarnation 0"
        );
        let mut f = b.clone();
        f[11] |= 0x80;
        assert!(VoiceEnvelope::decode(&f).is_err(), "reserved flag");
        let mut big = env(1);
        big.payload = vec![0; MAX_VOICE_PAYLOAD + 1];
        assert!(VoiceEnvelope::decode(&big.encode()).is_err());
    }

    #[test]
    fn empty_payload_is_legal_only_with_talkspurt_end() {
        let mut e = env(3);
        e.payload = vec![];
        assert!(VoiceEnvelope::decode(&e.encode()).is_err(), "empty, no END");
        e.flags = voice_flags::TALKSPURT_END;
        assert_eq!(VoiceEnvelope::decode(&e.encode()).unwrap(), e);
        // a non-empty END packet is still fine
        e.payload = vec![1];
        assert_eq!(VoiceEnvelope::decode(&e.encode()).unwrap(), e);
    }

    #[test]
    fn fence_retires_and_rejects_old_streams() {
        let mut f = IncarnationFence::default();
        assert_eq!(f.judge(5), Fence::NewStream { previous: 0 });
        assert_eq!(f.judge(5), Fence::Current);
        assert_eq!(f.judge(9), Fence::NewStream { previous: 5 });
        assert_eq!(f.judge(5), Fence::Stale, "late packet of the old stream");
        assert_eq!(f.judge(9), Fence::Current);
        f.retire_current();
        assert_eq!(f.judge(9), Fence::Stale);
        assert_eq!(f.judge(0), Fence::Stale);
    }

    #[test]
    fn voice_state_round_trips() {
        let s = VoiceState {
            flags: voice_state::LISTENING | voice_state::MIC,
            incarnation: 42,
        };
        assert_eq!(VoiceState::decode(&s.encode()).unwrap(), s);
        assert!(VoiceState::decode(&[1, 4, 0, 0, 0, 0]).is_err());
    }
}
