extends SceneTree
# Exercise Godot's real XR viewport resizing without opening a headset runtime.
class SizedXR extends XRInterfaceExtension:
	var active := true
	var target_size := Vector2(3560, 3968)
	func _get_name() -> StringName: return &"Menu size regression"
	func _is_initialized() -> bool: return active
	func _get_render_target_size() -> Vector2: return target_size
	func _get_view_count() -> int: return 2
	func _uninitialize() -> void: active = false

var app
var failures: Array[String] = []
func _initialize() -> void:
	if OS.get_name() == "Linux" and OS.get_environment("XDG_DATA_HOME").is_empty():
		printerr("Menu tests modify saved controls; set XDG_DATA_HOME to an isolated test profile.")
		quit(2)
		return
	call_deferred("run")
func check(condition: bool, message: String) -> void:
	print("CHECK ", message, ": ", condition)
	if not condition: failures.append(message)
func click(control: Control) -> void:
	await click_pixel(control.get_global_rect().get_center())
func click_pixel(pixel: Vector2) -> void:
	if not app.menu.vr_mode:
		var position: Vector2 = app.menu.desktop_surface.get_global_transform() * pixel
		var motion := InputEventMouseMotion.new()
		motion.position = position
		motion.global_position = position
		root.push_input(motion, true)
		await process_frame
		for pressed in [true, false]:
			var event := InputEventMouseButton.new()
			event.button_index = MOUSE_BUTTON_LEFT
			event.pressed = pressed
			event.position = position
			event.global_position = position
			root.push_input(event, true)
			await process_frame
		return
	var local := Vector3((pixel.x / app.menu.PIXELS.x - 0.5) * app.menu.METERS.x, (0.5 - pixel.y / app.menu.PIXELS.y) * app.menu.METERS.y, 0)
	var target: Vector3 = app.menu.to_global(local)
	var origin: Vector3 = app.camera.global_position
	var direction := (target - origin).normalized()
	app.menu.point(origin, direction, true)
	await process_frame
	app.menu.point(origin, direction, false)
	await process_frame
func run() -> void:
	# Synthetic desktop events must not depend on the OS cursor/window focus.
	root.gui_embed_subwindows = true
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
	check(Input.mouse_mode == Input.MOUSE_MODE_VISIBLE, "menu click keeps desktop cursor visible")
	app._notification(Node.NOTIFICATION_WM_WINDOW_FOCUS_OUT)
	app._notification(Node.NOTIFICATION_WM_WINDOW_FOCUS_IN)
	check(Input.mouse_mode == Input.MOUSE_MODE_VISIBLE, "window refocus preserves menu cursor")
	check(app.locomotion_axes(true, false, Vector2(0.7, 0.8), Vector2.ZERO) == Vector3(0, 0.8, 0.7), "left-only controller maps forward and turn")
	check(app.locomotion_axes(false, true, Vector2.ZERO, Vector2(0.7, 0.8)) == Vector3(0, 0.8, 0.7), "right-only controller maps forward and turn")
	check(app.locomotion_axes(true, true, Vector2(0.7, 0.8), Vector2(-0.5, 0)) == Vector3(0.7, 0.8, -0.5), "two-controller layout retains strafe and independent turn")
	check(app.locomotion_axes(false, false, Vector2.ONE, Vector2.ONE) == Vector3.ZERO, "inactive controller axes are ignored")
	check(not app.session.is_active() and app.muted, "singleplayer and muted startup")
	var panel: Control = app.menu.viewport.get_child(0)
	check(panel.size.x <= app.menu.PIXELS.x and panel.size.y <= app.menu.PIXELS.y, "menu fits viewport")
	app.menu.select_tab("Movie")
	await process_frame
	await click(app.menu.url)
	check(app.menu.text_focused(), "desktop mouse focuses URL input")
	app.menu.url.clear()
	for character in "https://example.invalid/movie":
		var event := InputEventKey.new()
		event.pressed = true
		event.unicode = character.unicode_at(0)
		root.push_input(event, true)
	await process_frame
	check(app.menu.url.text == "https://example.invalid/movie", "desktop keyboard reaches 2D menu")
	var previous: bool = app.smooth_turn
	app.menu.select_tab("Comfort")
	await process_frame
	await click(app.menu.smooth)
	check(app.smooth_turn != previous, "desktop mouse toggles smooth turn")
	app.save_setting("comfort", "smooth_turn", previous)
	app.menu.tabs.current_tab = 0
	await process_frame
	var previous_receive := Vector3(app.voice_volume_percent, app.voice_near_radius, app.voice_far_radius)
	await click(app.menu.receive_volume)
	check(app.voice_volume_percent == app.menu.receive_volume.value, "desktop mouse adjusts receive volume")
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
	check(app.movie_near_radius == 6 and app.movie_far_radius == 18, "movie defaults to full volume through six meters")
	app.menu.movie_far.value = 4
	check(app.movie_near_radius == 3.5, "movie outer radius keeps a valid interval")
	app.menu.movie_near.value = 8
	check(app.movie_far_radius == 8.5, "movie inner radius expands outer radius")
	app.set_movie_settings(6, 18)
	var speaker = app.get_node("EmissiveScreen/LeftSpeaker")
	speaker.global_position = app.camera.global_position + Vector3(0, 0, -6)
	app.update_video_volume()
	check(speaker.volume_linear == 1 and speaker.attenuation_filter_db == 0 and not speaker.air_absorption, "movie near field is full volume without muffling")
	speaker.global_position = app.camera.global_position + Vector3(0, 0, -18)
	app.update_video_volume()
	check(speaker.volume_linear == 0, "movie outer radius reaches silence")
	var avatar = app.Avatar.new()
	avatar.nameplate = Label3D.new()
	avatar.talking_indicator = Label3D.new()
	avatar.add_child(avatar.nameplate)
	avatar.add_child(avatar.talking_indicator)
	avatar.update_talking(true, 0.01)
	check(avatar.talking and avatar.talking_indicator.visible, "decoded voice lights the talking indicator")
	avatar.update_talking(false, 0.3)
	check(not avatar.talking and not avatar.talking_indicator.visible, "talking indicator clears after silence")
	avatar.free()
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
	var avatar_tab: Control = app.menu.avatar_picker.get_parent()
	avatar_tab.get_parent().current_tab = avatar_tab.get_index()
	var mirrored := false
	for control in app.menu.avatar_picker.get_parent().get_children():
		if control is TextureRect: mirrored = control.flip_h
	check(mirrored, "avatar preview is mirrored horizontally")
	app.set_process(true)
	app.menu.open_at(app.camera)
	app.select_avatar("vita")
	app.menu.eye_height.value = 1.75
	await create_timer(0.6).timeout
	check(app.avatar_height == 1.75 and app.local_avatar.avatar_id == "vita", "avatar tab applies selection and height")
	app.start_calibration()
	check(app.menu.avatar_status.text.contains("desktop"), "desktop calibration explains fixed viewpoint")
	if not output.is_empty():
		app.menu.viewport.get_texture().get_image().save_png(output + "-avatar.png")
	app.playback.set_process(false)
	var sharing: Control = app.menu.relay_rows.get_parent()
	sharing.get_parent().current_tab = sharing.get_index()
	var viewers := {}
	for i in range(5):
		viewers[str(i)] = {"peer":str(i), "name":"Friend " + str(i), "path":"relay", "consent":"pending"}
	app.menu.update_media_share(true, {"hosted":true, "publication":{"id":"fixture"}, "viewers":viewers, "average_mbps":12.0})
	await create_timer(0.3).timeout
	check(sharing.get_global_rect().end.y <= app.menu.PIXELS.y, "five relay decisions fit sharing tab")
	var decisions := []
	app.menu.relay_decided.connect(func(id, peer, allow): decisions.append([id, peer, allow]))
	await click(app.menu.relay_rows.get_child(0).get_child(1))
	check(decisions == [["fixture", "0", true]], "relay approval targets viewer and share")
	if not output.is_empty(): app.menu.viewport.get_texture().get_image().save_png(output + "-sharing.png")
	app.menu.select_tab("Movie")
	await process_frame
	if not output.is_empty(): app.menu.viewport.get_texture().get_image().save_png(output + "-movie.png")
	var button_events: Array[int] = []
	var probe_button := Button.new()
	probe_button.text = "VR button probe"
	probe_button.pressed.connect(func():
		button_events.append(1)
		print("BUTTON_EVENT count=", button_events.size(), " vr=", app.menu.vr_mode))
	app.menu.url.get_parent().get_parent().add_child(probe_button)
	var choices: Array[int] = []
	var probe_choice := OptionButton.new()
	probe_choice.add_item("First")
	probe_choice.add_item("Second")
	probe_choice.item_selected.connect(func(index): choices.append(index))
	app.menu.url.get_parent().get_parent().add_child(probe_choice)
	# Change presentation from inside the button's input callback, as Enter/Leave VR does.
	app.menu.xr_toggled.disconnect(app.xr_lifecycle.toggle)
	app.menu.xr_toggled.connect(func(): app.menu.set_vr_mode(not app.menu.vr_mode))
	for cycle in range(3):
		app.menu.select_tab("Room")
		await process_frame
		await click(app.menu.xr_button)
		app.menu.select_tab("Movie")
		app.menu.open_at(app.camera)
		await process_frame
		check(app.menu.quad.visible and not app.menu.desktop_surface.visible and app.menu.quad.material_override.no_depth_test, "VR menu overlays geometry")
		await click(app.menu.url)
		check(app.menu.text_focused(), "VR ray focuses movie field after transition")
		var previous_stereo: bool = app.menu.stereo.button_pressed
		await click(app.menu.stereo)
		check(app.menu.stereo.button_pressed != previous_stereo, "VR ray activates button after desktop transition")
		app.menu.stereo.button_pressed = previous_stereo
		await click(probe_button)
		check(button_events.size() == cycle * 2 + 1, "VR ray completes ordinary button press and release")
		probe_choice.select(0)
		await click(probe_choice)
		await create_timer(0.4).timeout
		var popup := probe_choice.get_popup()
		await click_pixel(Vector2(popup.position) + Vector2(popup.size) * Vector2(0.5, 0.75))
		check(choices.size() == cycle * 2 + 1 and probe_choice.selected == 1, "VR ray selects embedded dropdown item")
		popup.hide()
		app.menu.select_tab("Room")
		await process_frame
		await click(app.menu.devices)
		check(app.menu.devices.get_popup().visible, "VR ray opens dropdown after desktop transition")
		app.menu.devices.get_popup().hide()
		await click(app.menu.xr_button)
		app.menu.select_tab("Movie")
		await process_frame
		await click(app.menu.url)
		check(app.menu.text_focused() and not app.menu.quad.visible and Input.mouse_mode == Input.MOUSE_MODE_VISIBLE, "desktop menu remains clickable after transition")
		await click(probe_button)
		check(button_events.size() == cycle * 2 + 2, "desktop button works after VR return")
		probe_choice.select(0)
		await click(probe_choice)
		await create_timer(0.4).timeout
		popup = probe_choice.get_popup()
		await click_pixel(Vector2(popup.position) + Vector2(popup.size) * Vector2(0.5, 0.75))
		check(choices.size() == cycle * 2 + 2 and probe_choice.selected == 1, "desktop dropdown selection works after VR return")
		popup.hide()
	app.set_process(false)
	var sized_xr := SizedXR.new()
	XRServer.add_interface(sized_xr)
	app.xr_lifecycle.interface = sized_xr
	app.xr_lifecycle.render_viewport = root # Exercise the legacy root-viewport exit repair explicitly.
	# No controller models/tracking exist on the size-only interface.
	app.xr_lifecycle.mode_changed.disconnect(app.set_xr_mode)
	app.xr_lifecycle.mode_changed.connect(app.menu.set_vr_mode)
	for cycle in range(3):
		# Include content scaling; restoring only the raw pixel size is insufficient.
		root.content_scale_factor = 1.0 + cycle * 0.25
		await process_frame
		var desktop_rect := root.get_visible_rect()
		var desktop_transform: Transform2D = app.menu.desktop_surface.get_global_transform()
		sized_xr.active = true
		sized_xr.target_size = Vector2(3560 + cycle * 100, 3968)
		XRServer.primary_interface = sized_xr
		root.use_xr = true
		check(root.get_visible_rect().size == sized_xr.target_size, "XR adopts headset viewport size")
		app.menu.set_vr_mode(true)
		app.xr_lifecycle.state = "xr"
		# Stop before rendering a fake XR frame. This calls Prim's real exit path;
		# runtime_lost only bypasses the real OpenXR request/exit-state handshake.
		await app.xr_lifecycle.stop(true)
		XRServer.primary_interface = null
		await process_frame
		check(root.get_visible_rect() == desktop_rect, "XR exit restores desktop viewport and content scaling")
		check(app.menu.desktop_surface.get_global_transform().is_equal_approx(desktop_transform), "XR exit restores desktop menu size and position")
		var prior_events := button_events.size()
		await click(probe_button)
		check(button_events.size() == prior_events + 1, "desktop button works after actual XR viewport resize")
	XRServer.remove_interface(sized_xr)
	print("MENU_RESULT ", JSON.stringify(failures))
	quit(0 if failures.is_empty() else 1)
