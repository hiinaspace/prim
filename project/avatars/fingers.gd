extends SkeletonModifier3D

const Hand = preload("res://avatars/hand_pose.gd")
var rotations: Array[Quaternion] = []
var masks := PackedInt32Array([0,0])
var curls := PackedFloat32Array([0,0,0,0,0,0,0,0,0,0])
var bones: Array[int] = []
var hands: Array[int] = []
var displayed: Array[Quaternion] = []
var initialized := false

func _ready() -> void:
	var sk := get_skeleton()
	for side in ["Left", "Right"]:
		hands.append(sk.find_bone(side + "Hand"))
		for suffix in Hand.NAMES:
			bones.append(sk.find_bone(side + suffix))
	for bone in bones:
		displayed.append(sk.get_bone_rest(bone).basis.get_rotation_quaternion() if bone >= 0 else Quaternion.IDENTITY)

func _process_modification_with_delta(delta: float) -> void:
	var sk := get_skeleton()
	if bones.size() != 30: return
	for i in range(30):
		var bone := bones[i]
		if bone < 0: continue
		var hand := hands[i / 15]
		var parent := sk.get_bone_parent(bone)
		var target := sk.get_bone_rest(bone).basis.get_rotation_quaternion()
		if rotations.size() == 30 and masks[i / 15] & (1 << (i % 15)):
			var world := sk.get_bone_global_pose(hand).basis.orthonormalized() * Basis(rotations[i])
			target = (sk.get_bone_global_pose(parent).basis.orthonormalized().inverse() * world).get_rotation_quaternion()
		else:
			var curl := curls[(i / 15) * 5 + (i % 15) / 3]
			target = target * Quaternion(Vector3.RIGHT, curl * 1.2)
		displayed[i] = displayed[i].slerp(target, 1.0 if not initialized else 1.0 - exp(-30.0 * delta))
		sk.set_bone_pose_rotation(bone, displayed[i])
	initialized = true
