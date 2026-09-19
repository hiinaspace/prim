extends SceneTree
const Loader = preload("res://avatars/runtime_vrm.gd")
const Driver = preload("res://avatars/driver.gd")
var failed := false
func _initialize() -> void: call_deferred("run")
func check(ok: bool, label: String) -> void:
	print("RUNTIME_VRM_CHECK ", label, ": ", ok)
	if not ok: failed = true
func run() -> void:
	var world := Node3D.new()
	root.add_child(world)
	var camera := Camera3D.new()
	world.add_child(camera)
	camera.position = Vector3(0, 1.0, 3)
	camera.cull_mask = Driver.REMOTE
	var light := DirectionalLight3D.new()
	world.add_child(light)
	light.rotation_degrees = Vector3(-30, -20, 0)
	var paths: Array[String] = []
	var supplied := OS.get_environment("PRIM_RUNTIME_VRM_FIXTURE")
	if not supplied.is_empty(): paths.append(supplied)
	else:
		for id in ["alicia", "vita"]:
			var path: String = "user://runtime-proof-" + id + ".vrm"
			DirAccess.copy_absolute("res://avatars/models/" + id + ".vrm", path)
			paths.append(ProjectSettings.globalize_path(path))
	for path in paths:
		check(not FileAccess.file_exists(path + ".import"), "raw input has no editor import sidecar")
		var result := Loader.load_scene(path)
		check(result.has("scene"), "raw runtime load " + path.get_file() + " " + str(result.get("error", "")))
		if not result.has("scene"): continue
		var body := Driver.new()
		world.add_child(body)
		check(body.configure("runtime_test", 1.6, false, result.scene), "configure runtime humanoid")
		var template_springs: int = 0
		for node in result.scene.template.find_children("*", "Node", true, false):
			if node.get_script() == preload("res://addons/vrm/vrm_secondary.gd"):
				template_springs += node.spring_bones.size()
		var live_springs: int = 0
		for node in body.spring_nodes: live_springs += node.spring_bones_internal.size()
		check(template_springs > 0 and live_springs == template_springs, "all imported springs initialize")
		var second := Driver.new()
		world.add_child(second)
		check(second.configure("runtime_repeat", 1.8, true, result.scene), "repeat runtime instance")
		var repeat_springs: int = 0
		for node in second.spring_nodes: repeat_springs += node.spring_bones_internal.size()
		check(repeat_springs == live_springs, "repeat instance retains springs")
		second.free()
		check(body.spring_nodes.size() > 0, "runtime springs loaded")
		check(body.fingers.bones.filter(func(b): return b >= 0).size() == 30, "runtime fingers normalized")
		check(body.model.find_children("*", "MeshInstance3D", true, false).any(func(mesh): return mesh.layers & Driver.REMOTE != 0), "third-person mesh layer retained")
		for secondary in body.spring_nodes:
			for spring in secondary.spring_bones_internal:
				for verlet in spring.verlets:
					check(verlet.initial_transform.is_equal_approx(body.skeleton.get_bone_global_pose(verlet.bone_idx)), "spring initializes from actual skeleton bone")
		for i in range(90):
			var poses: Array[Transform3D] = [Transform3D(Basis.IDENTITY, Vector3(sin(i * 0.03)*0.2,1.6,0)), Transform3D.IDENTITY, Transform3D.IDENTITY]
			body.apply_frame(poses, 4, [], PackedInt32Array([0,0]), PackedFloat32Array([0,0,0,0,0,0,0,0,0,0]))
			await process_frame
		check(body.reset_count == 1, "ordinary movement preserves springs")
		for bone in range(body.skeleton.get_bone_count()):
			check(body.skeleton.get_bone_global_pose(bone).is_finite(), "finite solved bone")
		if DisplayServer.get_name() != "headless":
			await RenderingServer.frame_post_draw
			var output := OS.get_environment("PRIM_TEST_OUTPUT")
			if not output.is_empty(): root.get_texture().get_image().save_png(output + "-" + path.get_file() + ".png")
		body.free()
		if supplied.is_empty(): DirAccess.remove_absolute(path)
	check(Loader.load_scene("res://project.godot").has("error"), "non-VRM input rejected")
	print("RUNTIME_VRM_RESULT ", "FAIL" if failed else "PASS")
	quit(1 if failed else 0)
