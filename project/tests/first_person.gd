extends SceneTree
var failed := false
func _initialize() -> void: call_deferred("run")
func check(ok: bool, label: String) -> void:
	print("CHECK ",label,": ",ok)
	if not ok: failed = true
func shot() -> Image:
	await RenderingServer.frame_post_draw
	return root.get_texture().get_image()
func run() -> void:
	var app = load("res://main.tscn").instantiate()
	root.add_child(app)
	app.menu.hide()
	await create_timer(0.5).timeout
	app.set_process(false)
	check(root.get_camera_3d() == app.camera, "desktop uses filtered first-person camera")
	for id in ["alicia", "vita"]:
		app.select_avatar(id)
		var frame: Dictionary = app.sample_avatar_frame()
		app.local_avatar.apply_frame(frame.poses,frame.tracked,frame.fingers,frame.masks,frame.curls)
		await create_timer(0.5).timeout
		var body: Image = await shot()
		app.local_avatar.hide()
		var empty: Image = await shot()
		var difference := 0.0
		var count := 0
		for y in range(0, body.get_height() / 2, 8):
			for x in range(body.get_width() / 4, body.get_width() * 3 / 4, 8):
				var a := body.get_pixel(x,y)
				var b := empty.get_pixel(x,y)
				difference += absf(a.r-b.r)+absf(a.g-b.g)+absf(a.b-b.b)
				count += 1
		check(difference / count < 0.001, id + " local head does not obstruct forward view")
		var output := OS.get_environment("PRIM_TEST_OUTPUT")
		if not output.is_empty(): body.save_png(output + "-" + id + ".png")
	print("FIRST_PERSON_TESTS_PASSED" if not failed else "FIRST_PERSON_TESTS_FAILED")
	quit(1 if failed else 0)
