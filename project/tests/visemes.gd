extends SceneTree
const Expressions = preload("res://avatars/expressions.gd")
var failed := false
func check(ok: bool, label: String) -> void:
	print("VISEME_CHECK ", label, ": ", ok)
	if not ok: failed = true
func _initialize() -> void: call_deferred("run")
func run() -> void:
	for id in ["alicia", "vita"]:
		var scene = load("res://avatars/models/" + id + ".vrm")
		var first = scene.instantiate()
		var second = scene.instantiate()
		root.add_child(first)
		root.add_child(second)
		var face := Expressions.new()
		var other := Expressions.new()
		face.configure(first)
		other.configure(second)
		var skeleton: Skeleton3D = first.find_children("*", "Skeleton3D", true, false)[0]
		var eye := skeleton.find_bone("LeftEye")
		var eye_before := skeleton.get_bone_pose_rotation(eye)
		for vowel in Expressions.VOWELS:
			check(not face.binds[vowel].is_empty(), id + " declares vowel " + str(vowel))
			var values := PackedFloat32Array()
			values.resize(15)
			values[vowel] = 1.0
			face.apply(values, 1.0)
			var binding: Array = face.binds[vowel][0]
			check(binding[0].get_blend_shape_value(binding[1]) > 0.9, id + " vowel moves")
			for bind in other.owned: check(is_zero_approx(bind[0].get_blend_shape_value(bind[1])), id + " instance isolated")
			face.reset()
		check(skeleton.get_bone_pose_rotation(eye).is_equal_approx(eye_before), id + " eye tracks untouched")
		var values := PackedFloat32Array()
		values.resize(15)
		values[7] = 1.0
		face.apply(values, 1.0)
		check(face.binds[7][0][0].get_blend_shape_value(face.binds[7][0][1]) > 0.2, id + " missing SS uses vowel mixture")
		values.fill(0)
		face.apply(values, 0.016)
		for bind in face.owned: check(is_zero_approx(bind[0].get_blend_shape_value(bind[1])), id + " stale closes immediately")
		values[10] = NAN
		face.apply(values, 1.0)
		for bind in face.owned: check(is_finite(bind[0].get_blend_shape_value(bind[1])), id + " nonfinite rejected")
		first.free()
		second.free()
	# A synthetic full set covers exports with no VRM expression groups, without
	# putting private licensed test models into the project/import cache/package.
	var model := Node3D.new()
	var mesh := MeshInstance3D.new()
	model.add_child(mesh)
	mesh.mesh = ArrayMesh.new()
	for name in Expressions.NAMES: mesh.mesh.add_blend_shape("vrc.v_" + name)
	var arrays := []
	arrays.resize(Mesh.ARRAY_MAX)
	arrays[Mesh.ARRAY_VERTEX] = PackedVector3Array([Vector3.ZERO, Vector3.RIGHT, Vector3.UP])
	var shapes: Array[Array] = []
	for i in range(15):
		var shape: Array = arrays.duplicate(true)
		shape[Mesh.ARRAY_VERTEX][0].z = (i + 1) * 0.01
		shapes.append(shape)
	mesh.mesh.add_surface_from_arrays(Mesh.PRIMITIVE_TRIANGLES, arrays, shapes)
	var full := Expressions.new()
	full.configure(model)
	check(full.authored.count(1) == 15, "full authored set found without VRM groups")
	var values := PackedFloat32Array()
	values.resize(15)
	values[2] = 1
	full.apply(values, 1)
	check(mesh.get_blend_shape_value(2) > 0.9 and mesh.get_blend_shape_value(12) == 0, "authored FF overrides synthetic mixture")
	full.reset()
	mesh.set_blend_shape_value(12, 0.1)
	var baseline_face := Expressions.new()
	baseline_face.configure(model)
	values.fill(0)
	values[2] = 0.5
	baseline_face.apply(values, 1)
	check(is_equal_approx(mesh.get_blend_shape_value(12), 0.1), "animation preserves neutral baseline")
	baseline_face.reset()
	check(is_equal_approx(mesh.get_blend_shape_value(12), 0.1) and mesh.get_blend_shape_value(2) == 0, "reset restores neutral baseline")
	model.free()
	await worker_check()
	print("VISEME_RESULT ", "FAIL" if failed else "PASS")
	quit(1 if failed else 0)

func worker_check() -> void:
	var path := OS.get_environment("PRIM_TEST_SPEECH_PCM")
	if path.is_empty():
		check(false, "PRIM_TEST_SPEECH_PCM must point to 48k mono f32 speech fixture")
		return
	var data := FileAccess.get_file_as_bytes(path).to_float32_array()
	check(data.size() > 48000, "real speech fixture loaded")
	var analyzer = ClassDB.instantiate("PrimVisemes")
	root.add_child(analyzer)
	var deadline := Time.get_ticks_msec() + 10000
	while analyzer.get_status() == "loading" and Time.get_ticks_msec() < deadline: await process_frame
	if OS.get_environment("PRIM_TEST_NO_ORT") == "1":
		check(analyzer.get_status().begins_with("unavailable"), "missing runtime fails safely")
		check(analyzer.get_weights("local").count(0.0) == 15, "missing runtime is neutral")
		analyzer.queue_free()
		return
	check(analyzer.get_status() == "ready", "model initialized")
	if analyzer.get_status() != "ready": analyzer.queue_free(); return
	var sender = ClassDB.instantiate("NetworkAudioSender")
	var stream = ClassDB.instantiate("AudioStreamNetwork")
	root.add_child(sender)
	sender.connect_loopback_stream(stream)
	analyzer.attach_local(sender)
	analyzer.attach_remote("peer", stream)
	var player := AudioStreamPlayer.new()
	player.stream = stream
	root.add_child(player)
	player.play()
	var varied := {}
	for index in range(0, data.size(), 960):
		var chunk: PackedFloat32Array = data.slice(index, mini(index + 960, data.size()))
		sender.push_pcm_mono(chunk)
		for i in range(4): analyzer.push_test_pcm("extra" + str(i), chunk, 48000)
		await create_timer(0.020).timeout
		var weights: PackedFloat32Array = analyzer.get_weights("local")
		var best := 0
		for i in range(15):
			if not is_finite(weights[i]) or weights[i] < 0 or weights[i] > 1: check(false, "bounded model output")
			if weights[i] > weights[best]: best = i
		if weights[best] > 0.5: varied[best] = true
	check(varied.size() > 4, "real speech produces varied visemes")
	check(analyzer.get_stats("local").get("hops", 0) > 100, "local capture tap advances")
	check(analyzer.get_stats("peer").get("hops", 0) > 100, "post-NetEq tap advances")
	for i in range(4): check(analyzer.get_stats("extra" + str(i)).get("hops", 0) > 100, "six-source worker advances")
	check(analyzer.get_stats("local").get("dropped_samples", -1) == 0 and analyzer.get_stats("peer").get("dropped_samples", -1) == 0, "live taps do not overflow")
	print("VISEME_WORKER_STATS ", analyzer.get_stats("local"), " remote=", analyzer.get_stats("peer"))
	analyzer.reset_source("local")
	check(analyzer.get_weights("local").count(0.0) == 15, "reset invalidates in-flight results")
	await create_timer(0.3).timeout
	check(analyzer.get_weights("extra0").count(0.0) == 15, "input expiry is neutral")
	analyzer.remove_source("peer")
	check(analyzer.get_weights("peer").count(0.0) == 15, "removed peer is neutral")
	player.stop()
	player.queue_free()
	sender.queue_free()
	analyzer.queue_free()
	await process_frame
