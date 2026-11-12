//! Fixed seats around a table (R0). Positions are in the listener's own seat frame:
//! the origin is the listener's seat, -Z points at the table centre, +X is right, and
//! heads are 1.6 m up. The tracking source's pose (also in this frame) moves the
//! listener inside it; with no pose the listener sits still at the origin facing -Z.

use std::collections::{BTreeSet, HashMap};

use room_core::ids::Key;

pub const TABLE_RADIUS_M: f32 = 1.5;
pub const HEAD_HEIGHT_M: f32 = 1.6;

/// How a `--seat` / `--seat-index` argument names a seat.
#[derive(Clone, Debug, PartialEq)]
pub enum SeatSel {
    /// Hex prefix of the member key (a full key is 64 hex digits).
    KeyPrefix(String),
    /// Index among the other members, sorted by key.
    Index(usize),
}

#[derive(Default)]
pub struct Seats {
    self_key: Key,
    others: BTreeSet<Key>,
    explicit: Vec<(SeatSel, f32)>,
    overrides: HashMap<Key, f32>,
}

impl Seats {
    pub fn new(self_key: Key, explicit: Vec<(SeatSel, f32)>) -> Seats {
        Seats {
            self_key,
            others: BTreeSet::new(),
            explicit,
            overrides: HashMap::new(),
        }
    }

    /// Learn of a member (from a `Peer` message or a voice packet).
    pub fn add(&mut self, k: Key) {
        if k != self.self_key {
            self.others.insert(k);
        }
    }

    fn explicit_azimuth(&self, k: &Key) -> Option<f32> {
        if let Some(a) = self.overrides.get(k) {
            return Some(*a);
        }
        let hex = k.hex();
        let idx = self.others.iter().position(|o| o == k);
        self.explicit.iter().find_map(|(sel, az)| match sel {
            SeatSel::KeyPrefix(p) if hex.starts_with(p.as_str()) => Some(*az),
            SeatSel::Index(i) if Some(*i) == idx => Some(*az),
            _ => None,
        })
    }

    /// Seat position of `k` in the listener's seat frame.
    pub fn position(&self, k: &Key) -> [f32; 3] {
        if let Some(az) = self.explicit_azimuth(k) {
            let a = az.to_radians();
            return [
                TABLE_RADIUS_M * a.sin(),
                HEAD_HEIGHT_M,
                -TABLE_RADIUS_M * a.cos(),
            ];
        }
        // Everyone (listener included) sits at an even angle around the table, in
        // sorted-key order, so each member has one place at the table for all listeners.
        let mut all: Vec<Key> = self.others.iter().copied().collect();
        if !all.contains(k) && *k != self.self_key {
            all.push(*k);
        }
        all.push(self.self_key);
        all.sort();
        let n = all.len() as f32;
        let me = all.iter().position(|x| *x == self.self_key).unwrap_or(0) as f32;
        let j = all.iter().position(|x| x == k).unwrap_or(0) as f32;
        let alpha = (360.0 * (j - me) / n).to_radians();
        [
            TABLE_RADIUS_M * alpha.sin(),
            HEAD_HEIGHT_M,
            -TABLE_RADIUS_M + TABLE_RADIUS_M * alpha.cos(),
        ]
    }
}

/// Rotate `v` by the inverse of unit quaternion `q = (x, y, z, w)`.
pub fn rotate_inverse(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    if n.is_nan() || n <= 1e-6 {
        return v;
    }
    let (qx, qy, qz, qw) = (-q[0] / n, -q[1] / n, -q[2] / n, q[3] / n);
    // v' = v + 2w (q x v) + 2 q x (q x v)
    let t = [
        2.0 * (qy * v[2] - qz * v[1]),
        2.0 * (qz * v[0] - qx * v[2]),
        2.0 * (qx * v[1] - qy * v[0]),
    ];
    [
        v[0] + qw * t[0] + (qy * t[2] - qz * t[1]),
        v[1] + qw * t[1] + (qz * t[0] - qx * t[2]),
        v[2] + qw * t[2] + (qx * t[1] - qy * t[0]),
    ]
}

/// Listener-local direction and distance to a seat.
pub fn local_direction(
    listener_pos: [f32; 3],
    listener_rot: [f32; 4],
    seat: [f32; 3],
) -> ([f32; 3], f32) {
    let d = [
        seat[0] - listener_pos[0],
        seat[1] - listener_pos[1],
        seat[2] - listener_pos[2],
    ];
    let dist = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    (rotate_inverse(listener_rot, d), dist)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(b: u8) -> Key {
        Key([b; 32])
    }

    #[test]
    fn two_members_face_each_other_across_the_table() {
        let mut s = Seats::new(k(1), vec![]);
        s.add(k(2));
        let p = s.position(&k(2));
        assert!(
            p[0].abs() < 1e-4 && (p[2] + 2.0 * TABLE_RADIUS_M).abs() < 1e-4,
            "{p:?}"
        );
    }

    #[test]
    fn explicit_azimuth_and_index_win() {
        let mut s = Seats::new(
            k(1),
            vec![
                (SeatSel::KeyPrefix("0202".into()), 90.0),
                (SeatSel::Index(1), -90.0),
            ],
        );
        s.add(k(2));
        s.add(k(3));
        let a = s.position(&k(2));
        assert!((a[0] - TABLE_RADIUS_M).abs() < 1e-4 && a[2].abs() < 1e-4);
        let b = s.position(&k(3));
        assert!((b[0] + TABLE_RADIUS_M).abs() < 1e-4);
    }

    #[test]
    fn yaw_turns_a_right_hand_seat_ahead() {
        // Listener yawed -90 degrees about +Y (Godot: positive yaw turns left) sees a
        // seat on the +X side straight ahead.
        let half = (-90f32).to_radians() / 2.0;
        let q = [0.0, half.sin(), 0.0, half.cos()];
        let (d, dist) = local_direction([0.0, 1.6, 0.0], q, [1.5, 1.6, 0.0]);
        assert!(d[0].abs() < 1e-4 && (d[2] + 1.5).abs() < 1e-4, "{d:?}");
        assert!((dist - 1.5).abs() < 1e-4);
    }
}
