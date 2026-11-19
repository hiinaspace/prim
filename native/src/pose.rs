//! `PrimPose`: GDScript's face of `prim_client::pose` (the `member/<id>/pose` record).
//! Static helpers only; Godot's own convention (metres, +Y up, -Z forward) is the wire's.

use godot::prelude::*;
use prim_client::pose::{self, Core, Pose};

fn pose_of(t: Transform3D) -> Pose {
    let q = t.basis.orthonormalized().get_quaternion();
    Pose { pos: [t.origin.x, t.origin.y, t.origin.z], rot: [q.x, q.y, q.z, q.w] }
}

fn transform_of(p: &Pose) -> Transform3D {
    Transform3D::new(
        Basis::from_quaternion(Quaternion::new(p.rot[0], p.rot[1], p.rot[2], p.rot[3])),
        Vector3::new(p.pos[0], p.pos[1], p.pos[2]),
    )
}

#[derive(GodotClass)]
#[class(base=RefCounted, init)]
pub struct PrimPose {
    base: Base<RefCounted>,
}

#[godot_api]
impl PrimPose {
    #[constant]
    const FLAG_LEFT: i32 = pose::FLAG_LEFT as i32;
    #[constant]
    const FLAG_RIGHT: i32 = pose::FLAG_RIGHT as i32;
    #[constant]
    const FLAG_VR: i32 = pose::FLAG_VR as i32;

    /// The 64-byte live record for head, left hand, right hand (room-frame transforms).
    #[func]
    fn encode(flags: i64, seq: i64, head: Transform3D, left: Transform3D, right: Transform3D) -> PackedByteArray {
        let c = Core {
            flags: flags as u8,
            seq: seq as u16,
            head: pose_of(head),
            left: pose_of(left),
            right: pose_of(right),
        };
        PackedByteArray::from(pose::encode(&c).as_slice())
    }

    /// {flags, seq, head, left, right} (Transform3D each), or {} for a bad record.
    #[func]
    fn decode(bytes: PackedByteArray) -> VarDictionary {
        let mut d = VarDictionary::new();
        if let Ok(c) = pose::decode(bytes.as_slice()) {
            d.set("flags", c.flags as i64);
            d.set("seq", c.seq as i64);
            d.set("head", transform_of(&c.head));
            d.set("left", transform_of(&c.left));
            d.set("right", transform_of(&c.right));
        }
        d
    }
}
