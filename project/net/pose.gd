class_name PrimPose
extends RefCounted

# v2: header(32), view + canonical wrists(84), 30 wrist-relative finger
# quaternions(480), ten fallback curls(40). All floats are little-endian float32.
const VERSION := 2
const LENGTH := 636
const EMPTY_CURLS := [0.0,0.0,0.0,0.0,0.0,0.0,0.0,0.0,0.0,0.0]

static func newer(sequence: int, previous: int) -> bool:
	var difference := (sequence - previous) & 0xffffffff
	return previous < 0 or (difference != 0 and difference < 0x80000000)

static func encode(sequence: int, poses: Array[Transform3D], tracked: int, fingers: Array[Quaternion] = [], masks := PackedInt32Array([0,0]), curls := PackedFloat32Array(EMPTY_CURLS), epoch: int = 0, reset_epoch: int = 0) -> PackedByteArray:
	if poses.size() != 3 or masks.size() != 2 or curls.size() != 10 or (fingers.size() != 0 and fingers.size() != 30): return PackedByteArray()
	var bytes := PackedByteArray()
	bytes.resize(LENGTH)
	for item in [[0,VERSION],[4,sequence],[8,tracked],[12,epoch],[16,reset_epoch],[20,Time.get_ticks_msec() & 0xffffffff],[24,masks[0]],[28,masks[1]]]:
		bytes.encode_u32(item[0], item[1])
	var offset := 32
	for pose in poses:
		var q := pose.basis.orthonormalized().get_rotation_quaternion()
		for value in [pose.origin.x,pose.origin.y,pose.origin.z,q.x,q.y,q.z,q.w]:
			bytes.encode_float(offset,value)
			offset += 4
	for i in range(30):
		var q := fingers[i] if fingers.size() == 30 else Quaternion.IDENTITY
		for value in [q.x,q.y,q.z,q.w]:
			bytes.encode_float(offset,value)
			offset += 4
	for value in curls:
		bytes.encode_float(offset,value)
		offset += 4
	return bytes

static func decode(bytes: PackedByteArray) -> Dictionary:
	if bytes.size() != LENGTH or bytes.decode_u32(0) != VERSION: return {}
	var tracked := bytes.decode_u32(8)
	if tracked & ~7 != 0 or bytes.decode_u32(24) > 32767 or bytes.decode_u32(28) > 32767: return {}
	for offset in range(32,LENGTH,4):
		if not is_finite(bytes.decode_float(offset)): return {}
	var poses: Array[Transform3D] = []
	var offset := 32
	for i in range(3):
		var position := Vector3(bytes.decode_float(offset),bytes.decode_float(offset+4),bytes.decode_float(offset+8))
		var q := read_quaternion(bytes,offset+12)
		if position.length() > 1000 or not valid_quaternion(q): return {}
		poses.append(Transform3D(Basis(q.normalized()),position))
		offset += 28
	var fingers: Array[Quaternion] = []
	for i in range(30):
		var q := read_quaternion(bytes,offset)
		if not valid_quaternion(q): return {}
		fingers.append(q.normalized())
		offset += 16
	var curls := PackedFloat32Array()
	for i in range(10):
		var value := bytes.decode_float(offset)
		if value < 0 or value > 1: return {}
		curls.append(value)
		offset += 4
	return {"sequence":bytes.decode_u32(4),"tracked":tracked,"poses":poses,"epoch":bytes.decode_u32(12),"reset_epoch":bytes.decode_u32(16),"stamp":bytes.decode_u32(20),"fingers":fingers,"masks":PackedInt32Array([bytes.decode_u32(24),bytes.decode_u32(28)]),"curls":curls}

static func valid_quaternion(q: Quaternion) -> bool:
	return q.is_finite() and q.length_squared() >= 0.5 and q.length_squared() <= 1.5

static func read_quaternion(bytes: PackedByteArray, offset: int) -> Quaternion:
	return Quaternion(bytes.decode_float(offset),bytes.decode_float(offset+4),bytes.decode_float(offset+8),bytes.decode_float(offset+12))
