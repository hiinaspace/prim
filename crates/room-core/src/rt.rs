//! The local realtime protocol (docs/room/VOICE.md; local API, provisional while every
//! client ships in the one `prim` binary).
//!
//! One `SOCK_SEQPACKET` message per [`RtMsg`], `type u8 || body`, little-endian, strict.
//! It carries only voice frames, listener poses and the lease handshake, so nothing on
//! it waits behind control traffic or bulk. Times are `mono_us`: CLOCK_MONOTONIC in
//! microseconds, the one clock domain shared by the daemon and its clients on a host.

use crate::ids::Key;
use crate::voice::VoiceEnvelope;
use crate::wire::{DecodeError, Reader, Writer};

pub const RT_PROTO: u16 = 1;
/// Largest message either side sends or accepts.
pub const RT_MAX_MESSAGE: usize = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Role {
    /// Exclusive per daemon: captures, sends and renders voice.
    Voice = 1,
    /// Exclusive per daemon: the listener's head pose at the source rate.
    TrackingSource = 2,
}

impl Role {
    fn from_u8(v: u8) -> Result<Role, DecodeError> {
        match v {
            1 => Ok(Role::Voice),
            2 => Ok(Role::TrackingSource),
            k => Err(DecodeError::Kind(k)),
        }
    }
}

pub mod refuse {
    /// Another session holds the lease and `takeover` was 0.
    pub const BUSY: u8 = 1;
    pub const VERSION: u8 = 2;
    pub const BAD_HELLO: u8 = 3;
}

pub mod revoke {
    /// A newer session took the lease over.
    pub const TAKEOVER: u8 = 1;
    /// Nothing arrived within the lease TTL.
    pub const EXPIRED: u8 = 2;
}

pub mod peer_flags {
    /// Connected, a member, and advertised the voice capability.
    pub const CONNECTED: u8 = 1 << 0;
    pub const LISTENING: u8 = 1 << 1;
    pub const MIC: u8 = 1 << 2;
}

/// Listener pose: metres and a unit quaternion (x, y, z, w), Godot convention
/// (−Z forward, +Y up, right-handed), in the room's seat frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub pos: [f32; 3],
    pub rot: [f32; 4],
}

#[derive(Clone, Debug, PartialEq)]
pub enum RtMsg {
    // client -> daemon
    Hello {
        proto: u16,
        role: Role,
        takeover: bool,
        /// Voice: the stream incarnation the client will send. Tracking: 0.
        incarnation: u32,
        /// `voice::voice_state` bits.
        flags: u8,
    },
    VoiceOut {
        /// When the client handed the packet to the socket.
        sent_mono_us: u64,
        env: VoiceEnvelope,
    },
    PoseOut {
        source_mono_us: u64,
        seq: u32,
        pose: Pose,
    },
    Keepalive,
    SetState {
        flags: u8,
        incarnation: u32,
    },
    // daemon -> client
    Welcome {
        proto: u16,
        self_key: Key,
        lease_ttl_ms: u32,
        session: u64,
    },
    Refused {
        reason: u8,
    },
    VoiceIn {
        from: Key,
        /// Stamped at the daemon's datagram receive.
        received_at_mono_us: u64,
        env: VoiceEnvelope,
    },
    Listener {
        source_mono_us: u64,
        forwarded_mono_us: u64,
        seq: u32,
        pose: Pose,
    },
    Peer {
        key: Key,
        flags: u8,
        incarnation: u32,
    },
    Revoked {
        reason: u8,
    },
}

fn put_pose(w: &mut Writer, p: &Pose) {
    for v in p.pos.iter().chain(p.rot.iter()) {
        w.u32(v.to_bits());
    }
}

fn get_pose(r: &mut Reader) -> Result<Pose, DecodeError> {
    let mut f = [0f32; 7];
    for v in f.iter_mut() {
        *v = f32::from_bits(r.u32()?);
        if !v.is_finite() {
            return Err(DecodeError::Field("non-finite pose"));
        }
    }
    Ok(Pose {
        pos: [f[0], f[1], f[2]],
        rot: [f[3], f[4], f[5], f[6]],
    })
}

impl RtMsg {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        match self {
            RtMsg::Hello {
                proto,
                role,
                takeover,
                incarnation,
                flags,
            } => {
                w.u8(0x01)
                    .u16(*proto)
                    .u8(*role as u8)
                    .u8(*takeover as u8)
                    .u32(*incarnation)
                    .u8(*flags);
            }
            RtMsg::VoiceOut { sent_mono_us, env } => {
                w.u8(0x02).u64(*sent_mono_us);
                env.encode_into(&mut w);
            }
            RtMsg::PoseOut {
                source_mono_us,
                seq,
                pose,
            } => {
                w.u8(0x03).u64(*source_mono_us).u32(*seq);
                put_pose(&mut w, pose);
            }
            RtMsg::Keepalive => {
                w.u8(0x04);
            }
            RtMsg::SetState { flags, incarnation } => {
                w.u8(0x05).u8(*flags).u32(*incarnation);
            }
            RtMsg::Welcome {
                proto,
                self_key,
                lease_ttl_ms,
                session,
            } => {
                w.u8(0x81)
                    .u16(*proto)
                    .raw(&self_key.0)
                    .u32(*lease_ttl_ms)
                    .u64(*session);
            }
            RtMsg::Refused { reason } => {
                w.u8(0x82).u8(*reason);
            }
            RtMsg::VoiceIn {
                from,
                received_at_mono_us,
                env,
            } => {
                w.u8(0x83).raw(&from.0).u64(*received_at_mono_us);
                env.encode_into(&mut w);
            }
            RtMsg::Listener {
                source_mono_us,
                forwarded_mono_us,
                seq,
                pose,
            } => {
                w.u8(0x84)
                    .u64(*source_mono_us)
                    .u64(*forwarded_mono_us)
                    .u32(*seq);
                put_pose(&mut w, pose);
            }
            RtMsg::Peer {
                key,
                flags,
                incarnation,
            } => {
                w.u8(0x85).raw(&key.0).u8(*flags).u32(*incarnation);
            }
            RtMsg::Revoked { reason } => {
                w.u8(0x86).u8(*reason);
            }
        }
        w.finish()
    }

    pub fn decode(b: &[u8]) -> Result<RtMsg, DecodeError> {
        if b.len() > RT_MAX_MESSAGE {
            return Err(DecodeError::TooLarge("rt message"));
        }
        let mut r = Reader::new(b);
        let m = match r.u8()? {
            0x01 => {
                let proto = r.u16()?;
                let role = Role::from_u8(r.u8()?)?;
                let takeover = match r.u8()? {
                    0 => false,
                    1 => true,
                    _ => return Err(DecodeError::Field("takeover")),
                };
                RtMsg::Hello {
                    proto,
                    role,
                    takeover,
                    incarnation: r.u32()?,
                    flags: r.u8()?,
                }
            }
            0x02 => RtMsg::VoiceOut {
                sent_mono_us: r.u64()?,
                env: VoiceEnvelope::decode_from(&mut r)?,
            },
            0x03 => RtMsg::PoseOut {
                source_mono_us: r.u64()?,
                seq: r.u32()?,
                pose: get_pose(&mut r)?,
            },
            0x04 => RtMsg::Keepalive,
            0x05 => RtMsg::SetState {
                flags: r.u8()?,
                incarnation: r.u32()?,
            },
            0x81 => RtMsg::Welcome {
                proto: r.u16()?,
                self_key: Key(r.array()?),
                lease_ttl_ms: r.u32()?,
                session: r.u64()?,
            },
            0x82 => RtMsg::Refused { reason: r.u8()? },
            0x83 => RtMsg::VoiceIn {
                from: Key(r.array()?),
                received_at_mono_us: r.u64()?,
                env: VoiceEnvelope::decode_from(&mut r)?,
            },
            0x84 => RtMsg::Listener {
                source_mono_us: r.u64()?,
                forwarded_mono_us: r.u64()?,
                seq: r.u32()?,
                pose: get_pose(&mut r)?,
            },
            0x85 => RtMsg::Peer {
                key: Key(r.array()?),
                flags: r.u8()?,
                incarnation: r.u32()?,
            },
            0x86 => RtMsg::Revoked { reason: r.u8()? },
            k => return Err(DecodeError::Kind(k)),
        };
        r.end()?;
        Ok(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::CODEC_OPUS_48K_MONO;

    #[test]
    fn every_message_round_trips() {
        let env = VoiceEnvelope {
            codec: CODEC_OPUS_48K_MONO,
            incarnation: 3,
            seq: 1,
            timestamp: 960,
            flags: 0,
            payload: vec![1, 2, 3],
        };
        let pose = Pose {
            pos: [0.0, 1.6, 0.0],
            rot: [0.0, 0.0, 0.0, 1.0],
        };
        let k = Key([7; 32]);
        for m in [
            RtMsg::Hello {
                proto: RT_PROTO,
                role: Role::Voice,
                takeover: true,
                incarnation: 9,
                flags: 3,
            },
            RtMsg::VoiceOut {
                sent_mono_us: 5,
                env: env.clone(),
            },
            RtMsg::PoseOut {
                source_mono_us: 6,
                seq: 2,
                pose,
            },
            RtMsg::Keepalive,
            RtMsg::SetState {
                flags: 1,
                incarnation: 4,
            },
            RtMsg::Welcome {
                proto: 1,
                self_key: k,
                lease_ttl_ms: 2000,
                session: 11,
            },
            RtMsg::Refused { reason: 1 },
            RtMsg::VoiceIn {
                from: k,
                received_at_mono_us: 8,
                env,
            },
            RtMsg::Listener {
                source_mono_us: 1,
                forwarded_mono_us: 2,
                seq: 3,
                pose,
            },
            RtMsg::Peer {
                key: k,
                flags: 7,
                incarnation: 5,
            },
            RtMsg::Revoked { reason: 2 },
        ] {
            let b = m.encode();
            assert_eq!(RtMsg::decode(&b).unwrap(), m);
            let mut t = b.clone();
            t.push(0);
            assert!(RtMsg::decode(&t).is_err());
        }
    }
}
