//! `member/<id>/pose`: the live head-and-hands record (C2).
//!
//! One live record (unsigned, latest wins, never forwarded; <= 200 B by CONTRACTS.md
//! section 3) per member, written at ~20 Hz. Poses are in the room frame, Godot
//! convention (metres, +Y up, -Z forward, right-handed, quaternion x y z w), in the order
//! head, left hand, right hand. "Left" and "right" are the person's own left and right.
//!
//! Layout, little-endian, 64 bytes:
//!
//! | off | size | field |
//! |---|---|---|
//! | 0 | 1 | version = 1 |
//! | 1 | 1 | flags: bit0 left hand tracked, bit1 right hand tracked, bit2 head is a headset (else a desktop camera) |
//! | 2 | 2 | seq (wraps) |
//! | 4 | 20 | head: x y z as i32 millimetres, then 4 x i16 quaternion (x y z w, scaled by 32767, w >= 0) |
//! | 24 | 20 | left hand, same |
//! | 44 | 20 | right hand, same |
//!
//! Fingers are not here: they go as app datagrams (a later round). An untracked hand is
//! still encoded (its last or rest pose) but flagged, so a viewer can hide it.

pub const VERSION: u8 = 1;
pub const LEN: usize = 64;
pub const FLAG_LEFT: u8 = 1;
pub const FLAG_RIGHT: u8 = 2;
pub const FLAG_VR: u8 = 4;
/// Live record cap (CONTRACTS.md section 3).
pub const LIVE_CAP: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub pos: [f32; 3],
    /// Unit quaternion x y z w.
    pub rot: [f32; 4],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Core {
    pub flags: u8,
    pub seq: u16,
    pub head: Pose,
    pub left: Pose,
    pub right: Pose,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    Length(usize),
    Version(u8),
    NonFinite,
    OutOfRange,
    BadRotation,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "bad pose record: {self:?}")
    }
}

fn put(out: &mut Vec<u8>, p: &Pose) {
    for v in p.pos {
        out.extend_from_slice(&((v * 1000.0).round() as i32).to_le_bytes());
    }
    let mut q = p.rot;
    let n = q.iter().map(|v| v * v).sum::<f32>().sqrt();
    if n > 0.0 && n.is_finite() {
        q.iter_mut().for_each(|v| *v /= n);
    } else {
        q = [0.0, 0.0, 0.0, 1.0];
    }
    if q[3] < 0.0 {
        q.iter_mut().for_each(|v| *v = -*v);
    }
    for v in q {
        out.extend_from_slice(&((v * 32767.0).round() as i16).to_le_bytes());
    }
}

pub fn encode(c: &Core) -> Vec<u8> {
    let mut out = Vec::with_capacity(LEN);
    out.push(VERSION);
    out.push(c.flags & 7);
    out.extend_from_slice(&c.seq.to_le_bytes());
    put(&mut out, &c.head);
    put(&mut out, &c.left);
    put(&mut out, &c.right);
    debug_assert_eq!(out.len(), LEN);
    out
}

fn get(b: &[u8]) -> Result<Pose, DecodeError> {
    let mut pos = [0f32; 3];
    for (i, p) in pos.iter_mut().enumerate() {
        let v = i32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap());
        // 100 km is far outside any room; reject rather than draw garbage.
        if v.unsigned_abs() > 100_000_000 {
            return Err(DecodeError::OutOfRange);
        }
        *p = v as f32 / 1000.0;
    }
    let mut rot = [0f32; 4];
    for (i, r) in rot.iter_mut().enumerate() {
        let v = i16::from_le_bytes(b[12 + i * 2..14 + i * 2].try_into().unwrap());
        *r = v as f32 / 32767.0;
    }
    let n = rot.iter().map(|v| v * v).sum::<f32>().sqrt();
    if !(0.5..=1.5).contains(&n) {
        return Err(DecodeError::BadRotation);
    }
    rot.iter_mut().for_each(|v| *v /= n);
    if pos.iter().chain(rot.iter()).any(|v| !v.is_finite()) {
        return Err(DecodeError::NonFinite);
    }
    Ok(Pose { pos, rot })
}

pub fn decode(b: &[u8]) -> Result<Core, DecodeError> {
    if b.len() != LEN {
        return Err(DecodeError::Length(b.len()));
    }
    if b[0] != VERSION {
        return Err(DecodeError::Version(b[0]));
    }
    Ok(Core {
        flags: b[1] & 7,
        seq: u16::from_le_bytes([b[2], b[3]]),
        head: get(&b[4..24])?,
        left: get(&b[24..44])?,
        right: get(&b[44..64])?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q_yaw(a: f32) -> [f32; 4] {
        [0.0, (a / 2.0).sin(), 0.0, (a / 2.0).cos()]
    }

    fn sample() -> Core {
        Core {
            flags: FLAG_LEFT | FLAG_RIGHT | FLAG_VR,
            seq: 513,
            head: Pose { pos: [1.2345, 1.7, -3.25], rot: q_yaw(1.0) },
            left: Pose { pos: [0.8, 1.5, -3.0], rot: [0.0, 0.0, 0.7071, 0.7071] },
            right: Pose { pos: [1.6, 1.0, -3.0], rot: q_yaw(-2.0) },
        }
    }

    #[test]
    fn size_is_well_inside_the_live_cap() {
        let b = encode(&sample());
        assert_eq!(b.len(), LEN);
        assert!(b.len() * 3 < LIVE_CAP);
    }

    #[test]
    fn round_trip_within_quantization() {
        let c = sample();
        let d = decode(&encode(&c)).unwrap();
        assert_eq!((d.flags, d.seq), (c.flags, c.seq));
        for (a, b) in [(c.head, d.head), (c.left, d.left), (c.right, d.right)] {
            for i in 0..3 {
                assert!((a.pos[i] - b.pos[i]).abs() <= 0.0006, "pos {a:?} {b:?}");
            }
            let dot: f32 = a.rot.iter().zip(b.rot.iter()).map(|(x, y)| x * y).sum();
            assert!(dot.abs() > 0.99999, "rot {a:?} {b:?}");
        }
    }

    #[test]
    fn left_and_right_do_not_swap() {
        let c = sample();
        let d = decode(&encode(&c)).unwrap();
        assert!(d.left.pos[0] < d.right.pos[0]);
        assert!((d.left.pos[1] - 1.5).abs() < 1e-3 && (d.right.pos[1] - 1.0).abs() < 1e-3);
    }

    #[test]
    fn q_and_minus_q_encode_the_same_rotation() {
        let mut c = sample();
        c.head.rot = [0.0, -(0.5f32).sin(), 0.0, -(0.5f32).cos()];
        let d = decode(&encode(&c)).unwrap();
        assert!(d.head.rot[3] >= 0.0);
        assert!(d.head.rot[1] > 0.0); // canonical sign: -q is the same rotation as q
    }

    #[test]
    fn rejects_bad_records() {
        let b = encode(&sample());
        assert_eq!(decode(&b[..63]), Err(DecodeError::Length(63)));
        let mut v = b.clone();
        v[0] = 2;
        assert_eq!(decode(&v), Err(DecodeError::Version(2)));
        let mut z = b.clone();
        z[16..24].fill(0); // zero quaternion
        assert_eq!(decode(&z), Err(DecodeError::BadRotation));
        let mut far = b.clone();
        far[4..8].copy_from_slice(&i32::MAX.to_le_bytes());
        assert_eq!(decode(&far), Err(DecodeError::OutOfRange));
    }

    #[test]
    fn non_finite_input_encodes_as_identity_not_garbage() {
        let mut c = sample();
        c.head.rot = [f32::NAN; 4];
        let d = decode(&encode(&c)).unwrap();
        assert_eq!(d.head.rot, [0.0, 0.0, 0.0, 1.0]);
    }
}
