extends SceneTree
var app
var failures := []
var output := OS.get_environment("PRIM_TEST_OUTPUT")
var role := OS.get_environment("PRIM_TEST_ROLE")
var probe: CompositorEffect
var desktop_probe: CompositorEffect
var replies := {}
var remote_pose := {}
func _initialize() -> void: run.call_deferred()
func check(value: bool, message: String) -> void:
	print("CHECK ", message, ": ", value)
	if not value: failures.append(message)
func wait_for(predicate: Callable, seconds := 12.0) -> bool:
	var end := Time.get_ticks_msec() + int(seconds * 1000)
	while not predicate.call() and Time.get_ticks_msec() < end: await process_frame
	return predicate.call()
func signal_runner(stage: String) -> void:
	FileAccess.open(output + ".stage", FileAccess.WRITE).store_string(stage)
	check(await wait_for(func(): return FileAccess.file_exists(output + "." + stage)), "runner " + stage)
func run() -> void:
	if role == "game":
		await game()
		return
	app = load("res://main.tscn").instantiate()
	root.add_child(app)
	root.gui_embed_subwindows = true
	app.session.pose_received.connect(func(_peer, bytes): remote_pose = app.Pose.decode(bytes))
	app.session.message_received.connect(func(peer, raw):
		var message = JSON.parse_string(raw)
		if not message is Dictionary: return
		if message.get("type") == "companion_probe":
			app.session.send_control(peer, JSON.stringify({"type":"companion_reply", "phase":message.phase, "tracked":remote_pose.get("tracked", 0), "sequence":remote_pose.get("seq", -1)}))
		elif message.get("type") == "companion_reply": replies[message.phase] = message)
	if role == "observer":
		app.toggle_connection()
		await wait_for(func(): return FileAccess.file_exists(output + ".stop"), 150)
		quit(0)
		return
	app.session.endpoint_ready.connect(func(): FileAccess.open(output + ".endpoint", FileAccess.WRITE).store_string(app.session.get_endpoint_info()))
	app.toggle_connection()
	check(await wait_for(func(): return FileAccess.file_exists(output + ".endpoint")), "local room endpoint ready")
	await signal_runner("start-observer")
	check(await wait_for(func(): return app.avatars.size() == 1), "second Prim client joined")
	var identities := [app.session.get_instance_id(), app.player.get_instance_id(), app.sender.get_instance_id()]
	var size := root.get_visible_rect().size
	desktop_probe = preload("res://tests/xr_render_probe.gd").new()
	var desktop_compositor := Compositor.new()
	desktop_compositor.compositor_effects = [desktop_probe]
	app.desktop_camera.compositor = desktop_compositor
	for cycle in range(2):
		app.xr_lifecycle.start()
		if not await wait_for(func(): return app.xr_lifecycle.state == "xr"):
			check(false, "overlay initialized")
			break
		if cycle == 1:
			await app.xr_lifecycle.set_overlay_above(true)
			check(await wait_for(func(): return app.xr_lifecycle.state == "xr"), "composition change restarts only XR")
		check(int(app.xr_lifecycle.interface.get_overlay_status().get("placement", -1)) == (10 if cycle == 1 else 1), "runtime receives cooperative or above-overlay placement")
		check(app.xr_lifecycle.overlay_active, "actual overlay session selected")
		check(not root.use_xr and app.xr_viewport.use_xr, "desktop window and XR viewport separated")
		check(await wait_for(func(): return app.sample_avatar_frame().tracked == 7), "physical head and both hands track")
		check(root.get_visible_rect().size == size, "desktop dimensions unchanged by XR entry")
		probe = preload("res://tests/xr_render_probe.gd").new()
		app.xr_camera.compositor.compositor_effects = [app.reveal_effect, probe]
		if cycle == 0:
			check(await wait_for(func(): return app.xr_lifecycle.room_primary), "overlay alone automatically supplies Prim room")
			check(await wait_for(func(): return probe.snapshot().get("frame", 0) > 3), "overlay renders stereo frames")
			check(probe.snapshot().get("buffers") == 2 and probe.snapshot().get("scene") == 2, "actual render buffers and scene are stereo")
			check(float(probe.snapshot().get("ipd", 0)) > 0.04, "overlay has physical eye separation")
			await check_desktop_menu_controls()
			await signal_runner("start-game")
		check(await wait_for(func(): return app.xr_lifecycle.observation.get("activity") == "other" and not app.xr_lifecycle.room_primary), "game automatically hides room")
		await create_timer(0.5).timeout
		var hidden_frames: int = probe.snapshot().get("frame", 0)
		var desktop_frames: int = desktop_probe.snapshot().get("frame", 0)
		await create_timer(0.6).timeout
		check(probe.snapshot().get("frame", 0) == hidden_frames, "hidden overlay skips headset scene drawing")
		check(desktop_probe.snapshot().get("frame", 0) > desktop_frames, "desktop preview still renders while headset hidden")
		check(app.sample_avatar_frame().tracked == 7, "hidden mode still tracks head and hands")
		var phase := "hidden-" + str(cycle)
		app.session.broadcast_control(JSON.stringify({"type":"companion_probe", "phase":phase}))
		check(await wait_for(func(): return replies.has(phase)), "observer responds during hidden tracking")
		check(int(replies.get(phase, {}).get("tracked", 0)) == 7, "observer receives tracked head and both hands during game")
		check(not app.menu.headset_input_allowed and not app.laser.visible, "game blocks Prim controller UI")
		app.toggle_menu(true)
		check(app.menu.desktop_surface.visible and not app.menu.quad.visible, "Tab menu is desktop only while game owns headset")
		app.menu.tabs.current_tab = 0
		await process_frame
		var before: bool = app.muted
		var button: Control = app.menu.mic_button
		var point := button.get_global_rect().get_center()
		var motion := InputEventMouseMotion.new()
		motion.position = point
		motion.global_position = point
		app.menu.forward_desktop_input(motion)
		await process_frame
		for pressed in [true, false]:
			var event := InputEventMouseButton.new()
			event.button_index = MOUSE_BUTTON_LEFT
			event.position = point
			event.global_position = point
			event.pressed = pressed
			app.menu.forward_desktop_input(event)
			await process_frame
		check(app.muted != before, "desktop menu button callback works during game")
		if not app.muted: app.toggle_microphone()
		app.toggle_menu(true)
		var head: Vector3 = app.camera.global_position
		var offset: Vector3 = app.rig.global_position
		app.move_body(Vector3(0.1,0,0))
		check(app.rig.global_position.distance_to(offset + Vector3(0.1,0,0)) < 0.001, "desktop movement translates playspace anchor")
		check(app.camera.global_position.distance_to(head + Vector3(0.1,0,0)) < 0.001, "physical head moves with room anchor")
		if cycle == 0: await check_peek_locomotion()
		# Exercise presentation independently of physical gesture input.
		app.set_process(false)
		app.xr_lifecycle.set_reveal_fraction(0.5)
		check(await wait_for(func(): return probe.snapshot().get("frame",0) > hidden_frames + 3), "partial reveal resumes stereo rendering")
		# Exclude all geometry so this pixel can only come from the environment sky.
		var saved_mask: int = app.xr_camera.cull_mask
		app.xr_camera.cull_mask = 0
		await process_frame
		await process_frame
		probe.request_color_sample()
		check(await wait_for(func(): return probe.snapshot().has("alphas")), "GPU color sample captured")
		var alphas: PackedFloat32Array = probe.snapshot().get("alphas", PackedFloat32Array())
		check(alphas.size() == 2 and absf(alphas[0] - 0.5) < 0.01 and absf(alphas[1] - 0.5) < 0.01, "partial reveal writes half alpha to both eyes")
		var brightness: PackedFloat32Array = probe.snapshot().get("brightness", PackedFloat32Array())
		check(brightness.size() == 2 and brightness[0] > 0.01 and brightness[1] > 0.01, "environment sky renders in both eyes with reveal alpha")
		app.xr_camera.cull_mask = saved_mask
		check(not app.xr_lifecycle.room_primary and not app.menu.headset_input_allowed, "peek never enables Prim controller UI")
		check(app.xr_lifecycle.interface.get_overlay_status().active, "reveal retains same overlay role")
		app.xr_lifecycle.set_reveal_fraction(0)
		await create_timer(0.4).timeout
		var after: int = probe.snapshot().get("frame", 0)
		await create_timer(0.4).timeout
		check(probe.snapshot().get("frame", 0) == after, "hiding again skips headset rendering")
		app.set_process(true)
		if cycle == 1:
			await signal_runner("stop-game")
			check(await wait_for(func(): return app.xr_lifecycle.room_primary), "game exit automatically returns interactive room")
			check(await wait_for(func(): return app.menu.headset_input_allowed), "normal Prim controls return after game")
		await app.xr_lifecycle.stop()
		check(not app.xr and not app.xr_viewport.use_xr and not root.use_xr, "Disable VR fully detaches headset")
		check(root.get_visible_rect().size == size, "desktop dimensions survive disable")
		check([app.session.get_instance_id(), app.player.get_instance_id(), app.sender.get_instance_id()] == identities, "room media and microphone objects survive handoff")
		check(app.audio_listener.get_parent() == app.desktop_camera, "desktop listener restored")
		app.menu.open_desktop()
		await process_frame
		check(app.menu.desktop_surface.visible, "desktop menu remains available after disable")
	FileAccess.open(output + ".json", FileAccess.WRITE).store_string(JSON.stringify({"failures":failures}))
	print("XR_COMPANION_RESULT ", JSON.stringify(failures))
	quit(0 if failures.is_empty() else 1)

func game() -> void:
	# A separate real OpenXR main session, not a mock interface or second overlay.
	var origin := XROrigin3D.new()
	var camera := XRCamera3D.new()
	origin.add_child(camera)
	root.add_child(origin)
	camera.make_current()
	var life = preload("res://xr/session_lifecycle.gd").new()
	root.add_child(life)
	life.start()
	if not await wait_for(func(): return life.state == "xr"):
		quit(1)
		return
	check(not life.overlay_active, "reference game is a main session")
	FileAccess.open(output + ".ready", FileAccess.WRITE).store_string("ready")
	await wait_for(func(): return FileAccess.file_exists(output + ".stop"), 90)
	await life.stop()
	FileAccess.open(output + ".closed", FileAccess.WRITE).store_string("closed")
	quit(0)

# Inject a controller input source while preserving the real runtime/head tracking.
func check_desktop_menu_controls() -> void:
	var original_tracker: StringName = app.left.tracker
	var test_tracker := XRControllerTracker.new()
	test_tracker.type = XRServer.TRACKER_CONTROLLER
	test_tracker.name = &"/test/menu-controller"
	test_tracker.set_pose(app.left.pose, Transform3D.IDENTITY, Vector3.ZERO, Vector3.ZERO, XRPose.XR_TRACKING_CONFIDENCE_HIGH)
	XRServer.add_tracker(test_tracker)
	app.left.tracker = test_tracker.name
	app.menu.open_desktop()
	check(await wait_for(func(): return app.left.get_has_tracking_data() and app.controller_neutral and app.menu_ready), "test controller tracked and neutral")
	var start: Vector3 = app.rig.global_position
	test_tracker.set_input("primary", Vector2(0, 1))
	await create_timer(0.1).timeout
	check(app.rig.global_position.distance_to(start) > 0.05, "VR joystick works with desktop menu open")
	test_tracker.set_input("primary", Vector2.ZERO)
	test_tracker.set_input("by_button", true)
	await process_frame
	await process_frame
	check(app.menu.quad.visible and not app.menu.desktop_surface.visible, "VR menu button transfers desktop panel into headset")
	test_tracker.set_input("by_button", false)
	await process_frame
	app.menu.visible = false
	app.left.tracker = original_tracker
	XRServer.remove_tracker(test_tracker)

func check_peek_locomotion() -> void:
	var original: StringName = app.left.tracker
	var tracker := XRControllerTracker.new()
	tracker.type = XRServer.TRACKER_CONTROLLER
	tracker.name = &"/test/peek-controller"
	tracker.set_pose(app.left.pose, Transform3D.IDENTITY, Vector3.ZERO, Vector3.ZERO, XRPose.XR_TRACKING_CONFIDENCE_HIGH)
	tracker.set_input("primary", Vector2(0, 1))
	XRServer.add_tracker(tracker)
	app.left.tracker = tracker.name
	app.xr_lifecycle.show_reveal()
	check(await wait_for(func(): return app.xr_lifecycle.presentation_fraction > 0.9 and app.left.get_has_tracking_data()), "peek controls acquire tracked controller")
	var before: Vector3 = app.rig.global_position
	await create_timer(0.12).timeout
	check(app.rig.global_position.distance_to(before) < 0.001, "held gameplay stick cannot move Prim on peek entry")
	tracker.set_input("primary", Vector2.ZERO)
	await process_frame
	await process_frame
	tracker.set_input("primary", Vector2(0, 1))
	await create_timer(0.12).timeout
	check(app.rig.global_position.distance_to(before) > 0.05, "neutral then stick moves Prim during peek")
	tracker.set_input("primary", Vector2.ZERO)
	var muted: bool = app.muted
	tracker.set_input("ax_button", true)
	tracker.set_input("by_button", true)
	await process_frame
	await process_frame
	check(app.muted == muted and not app.menu.quad.visible and not app.laser.visible, "peek keeps mic menu and laser gameplay bindings disabled")
	app.reveal_gesture.latched = true
	app.reveal_gesture.fraction = 1.0
	app.xr_lifecycle.hide_reveal()
	await process_frame
	await process_frame
	check(not app.reveal_gesture.latched and app.xr_lifecycle.presentation_fraction == 0, "explicit hide clears a latched lift gesture")
	before = app.rig.global_position
	tracker.set_input("primary", Vector2(0, 1))
	await create_timer(0.12).timeout
	check(app.rig.global_position.distance_to(before) < 0.001, "hidden Prim ignores game locomotion")
	app.left.tracker = original
	XRServer.remove_tracker(tracker)
