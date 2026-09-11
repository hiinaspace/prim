extends SceneTree
var app
var failures: Array[String] = []
func _initialize() -> void: call_deferred("run")
func check(condition: bool, message: String) -> void:
	print("CHECK ", message, ": ", condition)
	if not condition: failures.append(message)
func click(control: Control) -> void:
	var pixel := control.get_global_rect().get_center()
	var local := Vector3((pixel.x / app.menu.PIXELS.x - 0.5) * app.menu.METERS.x, (0.5 - pixel.y / app.menu.PIXELS.y) * app.menu.METERS.y, 0)
	var target: Vector3 = app.menu.to_global(local)
	var origin: Vector3 = app.camera.global_position
	var direction := (target - origin).normalized()
	app.menu.point(origin, direction, true)
	await process_frame
	app.menu.point(origin, direction, false)
	await process_frame
func run() -> void:
	app = load("res://main.tscn").instantiate()
	root.add_child(app)
	await create_timer(1.0).timeout
	# The harness owns the synthetic ray; stop the live mouse ray from competing.
	app.set_process(false)
	check(not app.session.is_active() and app.muted, "singleplayer and muted startup")
	var panel: Control = app.menu.viewport.get_child(0)
	check(panel.size.x <= app.menu.PIXELS.x and panel.size.y <= app.menu.PIXELS.y, "menu fits viewport")
	await click(app.menu.url)
	check(app.menu.text_focused(), "world ray focuses URL input")
	app.menu.url.clear()
	for character in "https://example.invalid/movie":
		var event := InputEventKey.new()
		event.pressed = true
		event.unicode = character.unicode_at(0)
		app.menu.forward_key(event)
	await process_frame
	check(app.menu.url.text == "https://example.invalid/movie", "desktop typing reaches world menu")
	var previous: bool = app.smooth_turn
	await click(app.menu.smooth)
	check(app.smooth_turn != previous, "world ray toggles smooth turn")
	app.toggle_menu()
	check(not app.menu.visible and not app.menu.text_focused(), "closing menu releases text focus")
	app.toggle_menu()
	await process_frame
	var camera_position: Vector3 = app.camera.global_position
	app.rotate_about_head(0.5)
	check(camera_position.distance_to(app.camera.global_position) < 0.001, "turn pivots around head")
	app.rotate_about_head(-0.5)
	await create_timer(0.5).timeout
	var output := OS.get_environment("PRIM_TEST_OUTPUT")
	if not output.is_empty(): root.get_texture().get_image().save_png(output + ".png")
	print("MENU_RESULT ", JSON.stringify(failures))
	quit(0 if failures.is_empty() else 1)
