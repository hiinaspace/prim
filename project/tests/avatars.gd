extends SceneTree
const Driver = preload("res://avatars/driver.gd")
const Hand = preload("res://avatars/hand_pose.gd")
const Pose = preload("res://net/pose.gd")
var world: Node3D
var bodies: Array[PrimAvatarDriver] = []
var failed := false
var head_errors := []
var wrist_errors := []

func check(ok: bool, message: String) -> void:
	if not ok:
		failed = true
		push_error("AVATAR_CHECK: " + message)

func _initialize() -> void:
	call_deferred("run")

func run() -> void:
	world = Node3D.new()
	root.add_child(world)
	var camera := Camera3D.new()
	world.add_child(camera)
	camera.position = Vector3(0,1.15,3.7)
	camera.look_at(Vector3(0,0.95,0))
	camera.cull_mask = 1 | Driver.REMOTE
	camera.make_current()
	var light := DirectionalLight3D.new()
	light.rotation_degrees = Vector3(-40,-25,0)
	light.light_energy = 1.0
	world.add_child(light)
	var environment := WorldEnvironment.new()
	environment.environment = Environment.new()
	environment.environment.background_mode = Environment.BG_COLOR
	environment.environment.background_color = Color("253247")
	environment.environment.ambient_light_source = Environment.AMBIENT_SOURCE_COLOR
	environment.environment.ambient_light_color = Color.WHITE
	environment.environment.ambient_light_energy = 0.5
	world.add_child(environment)
	var ground := StaticBody3D.new()
	ground.collision_layer = 2
	var shape := CollisionShape3D.new()
	shape.shape = BoxShape3D.new()
	shape.shape.size = Vector3(20,0.2,20)
	shape.position.y = -0.1
	ground.add_child(shape)
	world.add_child(ground)
	var mesh := MeshInstance3D.new()
	mesh.mesh = PlaneMesh.new()
	mesh.mesh.size = Vector2(20,20)
	world.add_child(mesh)
	for id in ["alicia","vita"]:
		var body := Driver.new()
		world.add_child(body)
		check(body.configure(id,1.6,false), "configure " + id)
		check(body.spring_nodes.size() > 0, "spring nodes " + id)
		check(body.fingers.bones.filter(func(b): return b >= 0).size() == 30, "all finger bones " + id)
		check(body.spring_nodes[0].spring_bones_internal.size() > 0, "spring chains " + id)
		bodies.append(body)
	var fixture: Array[Transform3D] = []
	for tick in range(150):
		for i in range(bodies.size()):
			var x := -0.65 if i == 0 else 0.65
			var view := Transform3D(Basis(Vector3.UP,PI),Vector3(x,1.6,0))
			var left := Transform3D(Basis(Vector3.FORWARD,-PI/2),Vector3(x+0.38,1.12,0.2))
			var right := Transform3D(Basis(Vector3.FORWARD,PI/2),Vector3(x-0.38,1.12,0.2))
			fixture = [view,left,right]
			bodies[i].apply_frame(fixture,7,[],PackedInt32Array([0,0]),PackedFloat32Array([0,0,0,0,0,0.2,0.6,0.8,0.8,0.8]),tick==0)
		await process_frame
	for i in range(bodies.size()):
		var body := bodies[i]
		var sk := body.skeleton
		# Capture final modified bones in skeleton_updated, before Godot restores base poses.
		sk.skeleton_updated.connect(func():
			var head := sk.global_transform * sk.get_bone_global_pose(sk.find_bone("Head"))
			var error := head.origin.distance_to(body.head_target.global_position)
			head_errors.append(error)
			for hand in range(2):
				var bone := sk.find_bone("LeftHand" if hand == 0 else "RightHand")
				wrist_errors.append((sk.global_transform * sk.get_bone_global_pose(bone)).origin.distance_to(body.hand_targets[hand].global_position))
			for bone in range(sk.get_bone_count()):
				check(sk.get_bone_global_pose(bone).is_finite(), "finite bone pose")
		)
	await process_frame
	await process_frame
	print("HEAD_ERRORS ",head_errors," WRIST_ERRORS ",wrist_errors)
	check(not head_errors.is_empty() and head_errors.max() < 0.1, "head near tracked target")
	check(not wrist_errors.is_empty() and wrist_errors.max() < 0.2, "reachable wrist near target")
	if DisplayServer.get_name() != "headless":
		await RenderingServer.frame_post_draw
		root.get_texture().get_image().save_png(ProjectSettings.globalize_path("res://../.local/avatars-standing.png"))
	# Calibrated head targets carry uniform scale. A dangling foot must interpolate
	# rotation without Basis.slerp trying to cast that scale to a unit quaternion.
	for body in bodies:
		var placement = body.placement
		var scaled_head := Transform3D(Basis.from_euler(Vector3(0.2,0.4,0.1)).scaled(Vector3.ONE * 1.17), Vector3(0,1.8,0))
		placement.left_step.basis = scaled_head.basis
		placement.right_step.basis = scaled_head.basis
		placement.loop(scaled_head, Vector3(0,0,0.1), Vector3.ZERO, Vector3.UP, Vector3.ZERO, Vector3.UP, false, false, placement.forward_gait)
		check(placement.left_step.basis.is_finite() and absf(placement.left_step.basis.determinant()-1) < 0.001, "scaled left dangling foot produces a unit rotation")
		check(placement.right_step.basis.is_finite() and absf(placement.right_step.basis.determinant()-1) < 0.001, "scaled right dangling foot produces a unit rotation")
	# Synthetic OpenXR joints include thumb opposition and spreading, not just curl.
	var tracker := XRHandTracker.new()
	tracker.has_tracking_data = true
	var wrist := Basis(Vector3.UP, 0.7)
	tracker.set_hand_joint_flags(1, XRHandTracker.HAND_JOINT_FLAG_ORIENTATION_VALID)
	tracker.set_hand_joint_transform(1, Transform3D(wrist, Vector3.ZERO))
	var expected: Array[Quaternion] = []
	for i in range(15):
		var q := Quaternion(Vector3.UP, 0.15 + i * 0.01) * Quaternion(Vector3.RIGHT, 0.2)
		expected.append(q)
		tracker.set_hand_joint_flags(Hand.JOINTS[i], XRHandTracker.HAND_JOINT_FLAG_ORIENTATION_VALID)
		tracker.set_hand_joint_transform(Hand.JOINTS[i], Transform3D(wrist * Basis(q), Vector3.ZERO))
	var sampled := Hand.sample(tracker)
	check(sampled.mask == 32767, "tracked finger joint mask")
	for i in range(15): check(sampled.rotations[i].angle_to(expected[i]) < 0.001, "wrist-relative joint sampling")
	var rotations: Array[Quaternion] = []
	rotations.append_array(sampled.rotations)
	rotations.append_array(sampled.rotations)
	for body in bodies:
		body.fingers.rotations = rotations
		body.fingers.masks = PackedInt32Array([32767,32767])
	await create_timer(0.5).timeout
	var articulation_errors := []
	for body in bodies:
		var sk := body.skeleton
		sk.skeleton_updated.connect(func():
			for i in range(30):
				var actual := (sk.get_bone_global_pose(body.fingers.hands[i / 15]).basis.orthonormalized().inverse() * sk.get_bone_global_pose(body.fingers.bones[i]).basis.orthonormalized()).get_rotation_quaternion()
				articulation_errors.append(actual.angle_to(expected[i % 15]))
		, CONNECT_ONE_SHOT)
	await process_frame
	await process_frame
	check(articulation_errors.size() == 60 and articulation_errors.max() < 0.01, "both avatars preserve thumb and finger articulation")
	tracker.has_tracking_data = false
	check(Hand.sample(tracker).mask == 0, "lost hand tracking clears joint validity")
	# Identity/height changes and two independently stateful copies.
	var twin := Driver.new()
	world.add_child(twin)
	check(twin.configure("alicia",1.9,true), "second scaled instance")
	check(twin.model_scale > bodies[0].model_scale, "calibrated height scales avatar")
	check(twin.spring_nodes[0].spring_bones_internal[0] != bodies[0].spring_nodes[0].spring_bones_internal[0], "independent spring state")
	# Reliable configuration can arrive after its lossy pose. Unknown bundled IDs
	# must leave a usable fallback, and replacing the body must preserve anchors.
	var remote = preload("res://net/avatar.gd").new()
	world.add_child(remote)
	remote.set_process(false)
	remote.head = remote.box(Vector3.ONE * 0.2, Color.WHITE)
	remote.left = remote.box(Vector3.ONE * 0.1, Color.WHITE)
	remote.right = remote.box(Vector3.ONE * 0.1, Color.WHITE)
	var anchor = remote.head
	var packet := Pose.decode(Pose.encode(1, fixture, 7, [], PackedInt32Array([0,0]), PackedFloat32Array(Pose.EMPTY_CURLS), 1))
	remote.apply_frame(packet)
	check(not remote.pending_frame.is_empty() and remote.last_sequence == -1, "pose waits for matching configuration")
	remote.configure({"avatar":"alicia","revision":1,"eye_height":1.6,"epoch":1})
	check(remote.body != null and remote.last_sequence == 1, "configuration applies pending pose")
	remote.configure({"avatar":"unavailable","revision":1,"eye_height":1.6,"epoch":2})
	check(remote.body == null and remote.head == anchor and remote.head.layers == 1, "unknown ID uses cubes without replacing head anchor")
	remote.configure({"avatar":"vita","revision":1,"eye_height":1.6,"epoch":1})
	check(remote.body == null, "old configuration cannot overwrite newer state")
	remote.configure({"avatar":"vita","revision":2,"eye_height":1.6,"epoch":3})
	check(remote.body == null and remote.avatar_state.epoch == 3, "unknown catalog revision uses fallback")
	world.queue_free()
	await process_frame
	print("AVATAR_TESTS_PASSED" if not failed else "AVATAR_TESTS_FAILED")
	quit(1 if failed else 0)
