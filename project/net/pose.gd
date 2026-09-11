class_name PrimPose
extends RefCounted

# Version, sequence, tracking flags, then three world-space position/quaternion pairs.
const LENGTH := 96
static func encode(sequence: int, poses: Array[Transform3D], tracked: int) -> PackedByteArray:
	var bytes := PackedByteArray()
	bytes.resize(LENGTH)
	bytes.encode_u32(0, 1)
	bytes.encode_u32(4, sequence)
	bytes.encode_u32(8, tracked)
	var offset := 12
	for pose in poses:
		var q := pose.basis.orthonormalized().get_rotation_quaternion()
		for value in [pose.origin.x,pose.origin.y,pose.origin.z,q.x,q.y,q.z,q.w]:
			bytes.encode_float(offset, value)
			offset += 4
	return bytes

static func decode(bytes: PackedByteArray) -> Dictionary:
	if bytes.size() != LENGTH or bytes.decode_u32(0) != 1:
		return {}
	var poses: Array[Transform3D] = []
	var offset := 12
	for i in range(3):
		var values := []
		for j in range(7):
			var value := bytes.decode_float(offset)
			if not is_finite(value):
				return {}
			values.append(value)
			offset += 4
		var position := Vector3(values[0],values[1],values[2])
		var q := Quaternion(values[3],values[4],values[5],values[6])
		if position.length() > 1000 or q.length_squared() < 0.5 or q.length_squared() > 1.5:
			return {}
		poses.append(Transform3D(Basis(q.normalized()),position))
	return {"sequence":bytes.decode_u32(4),"tracked":bytes.decode_u32(8),"poses":poses}
