extends SceneTree
var failed := false
func _initialize() -> void: call_deferred("run")
func check(ok: bool, label: String) -> void:
	print("PRIVATE_VRM_CHECK ", label, ": ", ok)
	if not ok: failed = true
func run() -> void:
	# Run only inside the generated scratch project; no private assets in this repo.
	var app = load("res://main.tscn").instantiate()
	root.add_child(app)
	await create_timer(1).timeout
	check(app.local_avatar.model.find_children("*", "MeshInstance3D", true, false).any(func(mesh): return mesh.layers & app.local_avatar.LOCAL_THIRD != 0), "private mesh visible to mirror camera")
	check(app.avatar_id == "private_test", "private model selected")
	check(app.local_avatar.expressions.authored.count(1) == 15, "all fifteen authored shapes retained by importer")
	check(OS.get_user_data_dir().ends_with("prim-private-vrm-preview"), "settings isolated")
	app.toggle_connection()
	check(not app.session.is_active(), "preview stays offline")
	app.menu.avatar_close_up.button_pressed = true
	app.menu.avatar_picker.get_parent().get_parent().current_tab = 1
	app.menu.avatar_vowels_only.button_pressed = true
	check(app.local_avatar.expressions.authored.count(1) == 5, "comparison uses five authored vowels")
	check(not app.local_avatar.expressions.binds[2].is_empty(), "comparison synthesizes consonants")
	app.menu.avatar_vowels_only.button_pressed = false
	check(app.local_avatar.expressions.authored.count(1) == 15, "full set restored")
	await create_timer(0.5).timeout
	# Freeze normal mouth updates, then inspect a known authored FF weight.
	app.set_process(false)
	app._process(0.016)
	var weights := PackedFloat32Array()
	weights.resize(15)
	weights[2] = 1
	app.local_avatar.apply_visemes(weights, 1)
	var bind: Array = app.local_avatar.expressions.binds[2][0]
	check(bind[0].get_blend_shape_value(bind[1]) > 0.5, "authored FF moves")
	await process_frame
	await RenderingServer.frame_post_draw
	var output := OS.get_environment("PRIM_PRIVATE_VRM_SCREENSHOT")
	if not output.is_empty(): app.menu.viewport.get_texture().get_image().save_png(output)
	print("PRIVATE_VRM_RESULT ", "FAIL" if failed else "PASS")
	quit(1 if failed else 0)
