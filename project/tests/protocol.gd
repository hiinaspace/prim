extends SceneTree
const Pose = preload("res://net/pose.gd")
const Playback = preload("res://media/playback.gd")
func _initialize() -> void:
	var poses: Array[Transform3D] = [Transform3D(Basis(Vector3.UP, 0.7), Vector3(1,2,3)), Transform3D.IDENTITY, Transform3D.IDENTITY]
	var encoded := Pose.encode(42, poses, 3)
	var decoded := Pose.decode(encoded)
	assert(decoded.sequence == 42 and decoded.tracked == 3)
	assert(decoded.poses[0].is_equal_approx(poses[0]))
	encoded.encode_float(12, NAN)
	assert(Pose.decode(encoded).is_empty())
	assert(Pose.decode(PackedByteArray([0,1])).is_empty())
	assert(Playback.target_position({"position":10.0,"stamp":50.0,"paused":false}, 53.0) == 13.0)
	assert(Playback.target_position({"position":10.0,"stamp":50.0,"paused":true}, 53.0) == 10.0)
	assert(not Playback.valid_snapshot({"position":NAN}))
	print("PROTOCOL_TESTS_PASSED")
	quit()
