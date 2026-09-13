class_name PrimHandPose
extends RefCounted

# Wrist-relative orientations in Godot's humanoid axes, independent of helpers
# in an imported skeleton. Three joints per finger, thumb through little finger.
const JOINTS := [2,3,4,7,8,9,12,13,14,17,18,19,22,23,24]
const NAMES := ["ThumbMetacarpal","ThumbProximal","ThumbDistal", "IndexProximal","IndexIntermediate","IndexDistal", "MiddleProximal","MiddleIntermediate","MiddleDistal", "RingProximal","RingIntermediate","RingDistal", "LittleProximal","LittleIntermediate","LittleDistal"]

static func sample(tracker: XRHandTracker) -> Dictionary:
	var rotations: Array[Quaternion] = []
	rotations.resize(15)
	rotations.fill(Quaternion.IDENTITY)
	var mask := 0
	if tracker and tracker.has_tracking_data and tracker.get_hand_joint_flags(1) & XRHandTracker.HAND_JOINT_FLAG_ORIENTATION_VALID:
		var wrist := tracker.get_hand_joint_transform(1).basis.orthonormalized()
		for i in range(15):
			if tracker.get_hand_joint_flags(JOINTS[i]) & XRHandTracker.HAND_JOINT_FLAG_ORIENTATION_VALID:
				var q := (wrist.inverse() * tracker.get_hand_joint_transform(JOINTS[i]).basis.orthonormalized()).get_rotation_quaternion()
				if q.is_finite():
					rotations[i] = q.normalized()
					mask |= 1 << i
	return {"rotations":rotations, "mask":mask}

static func controller_basis(left_hand: bool) -> Basis:
	# Same grip-to-humanoid axis convention as FPSloppa (wrist +Y along fingers).
	var side := 1.0 if left_hand else -1.0
	return Basis(Vector3.BACK * side, Vector3.DOWN, Vector3.RIGHT * side)

static func wrist(rig: XROrigin3D, controller: XRController3D, tracker: XRHandTracker, left_hand: bool) -> Dictionary:
	if tracker and tracker.has_tracking_data:
		var flags := tracker.get_hand_joint_flags(1)
		if flags & XRHandTracker.HAND_JOINT_FLAG_POSITION_VALID and flags & XRHandTracker.HAND_JOINT_FLAG_ORIENTATION_VALID:
			return {"pose":rig.global_transform * tracker.get_hand_joint_transform(1), "valid":true}
	var pose := controller.global_transform
	# Small grip-to-wrist shift, deliberately adjustable as in Mainspring.
	pose.origin += pose.basis.y * 0.06
	pose.basis = pose.basis * controller_basis(left_hand)
	return {"pose":pose, "valid":controller.get_has_tracking_data()}

static func curls(controller: XRController3D) -> PackedFloat32Array:
	var grip := clampf(controller.get_float("grip"), 0, 1)
	var trigger := clampf(controller.get_float("trigger"), 0, 1)
	var thumb := controller.is_button_pressed("ax_button") or controller.is_button_pressed("by_button") or controller.is_button_pressed("primary_touch")
	return PackedFloat32Array([0.7 if thumb else 0.0, trigger, grip, grip, grip])
