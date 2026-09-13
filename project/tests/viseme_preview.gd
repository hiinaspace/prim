extends SceneTree
const Expressions = preload("res://avatars/expressions.gd")
func _initialize() -> void: call_deferred("run")
func run() -> void:
	var output := Image.create(1152, 768, false, Image.FORMAT_RGBA8)
	for row in range(2):
		for col in range(3):
			var viewport := SubViewport.new()
			viewport.size = Vector2i(384,384)
			viewport.own_world_3d = true
			viewport.render_target_update_mode = SubViewport.UPDATE_ALWAYS
			root.add_child(viewport)
			var model = load("res://avatars/models/" + ["alicia", "vita"][row] + ".vrm").instantiate()
			viewport.add_child(model)
			var face := Expressions.new()
			face.configure(model)
			var values := PackedFloat32Array()
			values.resize(15)
			if col > 0: values[[0,10,7][col]] = 1.0
			face.apply(values, 1.0)
			var skeleton: Skeleton3D = model.find_children("*", "Skeleton3D", true, false)[0]
			var target := skeleton.to_global(skeleton.get_bone_global_pose(skeleton.find_bone("Head")).origin) + Vector3(0,0.07,0)
			var camera := Camera3D.new()
			viewport.add_child(camera)
			camera.position = target + Vector3(0,0,0.7)
			camera.look_at(target)
			camera.fov = 32
			camera.make_current()
			var light := DirectionalLight3D.new()
			light.rotation_degrees = Vector3(-25,-20,0)
			viewport.add_child(light)
			var env := WorldEnvironment.new()
			env.environment = Environment.new()
			env.environment.background_mode = Environment.BG_COLOR
			env.environment.background_color = Color("253247")
			env.environment.ambient_light_source = Environment.AMBIENT_SOURCE_COLOR
			env.environment.ambient_light_color = Color.WHITE
			env.environment.ambient_light_energy = 0.7
			viewport.add_child(env)
			await process_frame
			await RenderingServer.frame_post_draw
			output.blit_rect(viewport.get_texture().get_image(), Rect2i(0,0,384,384), Vector2i(col*384,row*384))
			viewport.free()
	output.save_png(OS.get_environment("PRIM_VISEME_PREVIEW"))
	quit()
