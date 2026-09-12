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
	Input.mouse_mode = Input.MOUSE_MODE_VISIBLE
	var recapture := InputEventMouseButton.new()
	recapture.button_index = MOUSE_BUTTON_LEFT
	recapture.pressed = true
	app._input(recapture)
	check(Input.mouse_mode == Input.MOUSE_MODE_CAPTURED and app.ignore_pointer_until_release, "click recaptures desktop cursor without activating menu")
	app._notification(Node.NOTIFICATION_WM_WINDOW_FOCUS_OUT)
	app._notification(Node.NOTIFICATION_WM_WINDOW_FOCUS_IN)
	check(Input.mouse_mode == Input.MOUSE_MODE_CAPTURED, "window refocus recaptures cursor")
	check(app.locomotion_axes(true, false, Vector2(0.7, 0.8), Vector2.ZERO) == Vector3(0, 0.8, 0.7), "left-only controller maps forward and turn")
	check(app.locomotion_axes(false, true, Vector2.ZERO, Vector2(0.7, 0.8)) == Vector3(0, 0.8, 0.7), "right-only controller maps forward and turn")
	check(app.locomotion_axes(true, true, Vector2(0.7, 0.8), Vector2(-0.5, 0)) == Vector3(0.7, 0.8, -0.5), "two-controller layout retains strafe and independent turn")
	check(app.locomotion_axes(false, false, Vector2.ONE, Vector2.ONE) == Vector3.ZERO, "inactive controller axes are ignored")
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
	app.save_setting("comfort", "smooth_turn", previous)
	var previous_receive := Vector3(app.voice_volume_percent, app.voice_near_radius, app.voice_far_radius)
	await click(app.menu.receive_volume)
	check(app.voice_volume_percent == app.menu.receive_volume.value, "world ray adjusts receive volume")
	app.menu.voice_far.value = 1.0
	check(app.voice_far_radius > app.voice_near_radius, "shrinking outer radius keeps a valid falloff interval")
	app.menu.voice_near.value = 12.0
	check(app.voice_far_radius >= 12.5, "growing inner radius expands outer radius")
	var saved := ConfigFile.new()
	saved.load("user://settings.cfg")
	check(saved.get_value("audio", "voice_near_radius") == app.voice_near_radius and saved.get_value("audio", "receive_volume") == app.voice_volume_percent, "receive preferences are saved locally")
	app.set_voice_settings(previous_receive.x, previous_receive.y, previous_receive.z)
	app.toggle_menu()
	check(not app.menu.visible and not app.menu.text_focused(), "closing menu releases text focus")
	app.toggle_menu()
	await process_frame
	var camera_position: Vector3 = app.camera.global_position
	app.rotate_about_head(0.5)
	check(camera_position.distance_to(app.camera.global_position) < 0.001, "turn pivots around head")
	app.rotate_about_head(-0.5)
	app.camera.position = Vector3(2.0, 1.6, 1.0)
	camera_position = app.camera.global_position
	for i in range(12):
		app.rotate_about_head(0.5)
		app.move_body(Vector3.ZERO)
	check(camera_position.distance_to(app.camera.global_position) < 0.001, "room-scale turns preserve offset head through bounds handling")
	app.move_body(Vector3(0.1, 0, 0))
	check((camera_position + Vector3(0.1, 0, 0)).distance_to(app.camera.global_position) < 0.001, "movement translates the offset body")
	app.move_body(Vector3(100, 0, 100))
	check(absf(app.to_local(app.camera.global_position).x - 8.6) < 0.001 and absf(app.to_local(app.camera.global_position).z - 6.95) < 0.001, "movement bounds constrain body rather than playspace origin")
	check(app.movie_attenuation_db(0) == 0 and app.movie_attenuation_db(3) == 0, "movie speakers retain full near-field volume")
	check(app.movie_attenuation_db(6) == -6 and app.movie_attenuation_db(9) == -12, "movie speakers decay exponentially beyond three meters")
	app.rig.transform = Transform3D(Basis(), Vector3(0, 0, 3))
	app.camera.position = Vector3(0, 1.6, 0)
	app.playback.set_process(false)
	app.menu.update_playback(30, 120, true)
	var requested: Array[float] = []
	app.menu.seek_requested.connect(func(value): requested.append(value))
	app.menu.timeline.value = 50
	app.menu.timeline.value = 60
	await create_timer(0.3).timeout
	check(requested.size() == 1 and requested[0] == 60, "scrubbing debounces to one absolute seek")
	app.menu.update_playback(0, 0, false)
	await create_timer(0.3).timeout
	check(requested.size() == 1 and not app.menu.timeline.editable, "unavailable duration resets without seeking")
	var visual := preload("res://xr/controller_visual.gd").new()
	var fake := Node3D.new()
	var mesh := MeshInstance3D.new()
	mesh.mesh = BoxMesh.new()
	fake.add_child(mesh)
	root.add_child(fake)
	visual.models.append(fake)
	check(visual.has_visible_model(), "controller fallback recognizes a supplied mesh")
	mesh.visible = false
	check(not visual.has_visible_model(), "controller fallback survives absent or hidden provider geometry")
	fake.free()
	visual.free()
	await create_timer(0.5).timeout
	var output := OS.get_environment("PRIM_TEST_OUTPUT")
	if not output.is_empty(): root.get_texture().get_image().save_png(output + ".png")
	print("MENU_RESULT ", JSON.stringify(failures))
	quit(0 if failures.is_empty() else 1)
