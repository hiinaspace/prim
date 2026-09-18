extends SceneTree

var app
var role := OS.get_environment("PRIM_TEST_ROLE")
var output := OS.get_environment("PRIM_TEST_OUTPUT")
var failures: Array[String] = []
var replies := {}
var finished := false
var entries := 0
var render_probe: CompositorEffect
var desktop_render_probe: CompositorEffect
var peer := ""
var identities: Array[int] = []
var endpoint := ""

func _initialize() -> void: run.call_deferred()

func check(value: bool, message: String) -> void:
	print("CHECK ", message, ": ", value)
	if not value: failures.append(message)

func wait_for(predicate: Callable, seconds := 15.0) -> bool:
	var until := Time.get_ticks_msec() + int(seconds * 1000)
	while not predicate.call() and Time.get_ticks_msec() < until: await process_frame
	return predicate.call()

func signal_runner(stage: String) -> void:
	FileAccess.open(output + ".stage", FileAccess.WRITE).store_string(stage)
	check(await wait_for(func(): return FileAccess.file_exists(output + "." + stage), 15), "runner " + stage)

func receive(id: String, raw: String) -> void:
	var message: Variant = JSON.parse_string(raw)
	if not message is Dictionary: return
	if message.get("type") == "xr_probe":
		app.session.send_control(id, JSON.stringify({"type":"xr_reply", "phase":message.phase, "position":app.player.get_playback_position(), "loaded":app.playback.loaded, "voice":app.session.receive_stream(id).get_stats().get("non_silent_output_frames", 0), "poses":app.avatars[id].last_sequence}))
	elif message.get("type") == "xr_reply": replies[message.phase] = message
	elif message.get("type") == "xr_finish": finish()

func probe(phase: String) -> Dictionary:
	app.session.broadcast_control(JSON.stringify({"type":"xr_probe", "phase":phase}))
	check(await wait_for(func(): return replies.has(phase), 5), "peer responds " + phase)
	return replies.get(phase, {})

func persistent_nodes() -> Array[int]:
	return [app.session.get_instance_id(), app.sender.get_instance_id(), app.player.get_instance_id(), app.playback.get_instance_id()]

func continuity(phase: String, before: Dictionary) -> Dictionary:
	await create_timer(1.0).timeout
	check(persistent_nodes() == identities and app.session.get_endpoint_info() == endpoint, "room/player/microphone identities survive " + phase)
	check(app.session.is_active() and app.avatars.has(peer) and not app.muted and app.sender.is_capturing(), "membership and microphone survive " + phase)
	check(app.audio_listener.get_parent() == app.camera and app.camera.is_current(), "listener follows active camera " + phase)
	var after := await probe(phase)
	check(after.get("loaded", false) and float(after.get("position", 0)) > float(before.get("position", 0)), "shared movie advances " + phase)
	check(float(after.get("voice", 0)) > float(before.get("voice", 0)) + 10000, "peer receives voice " + phase)
	check(float(after.get("poses", 0)) > float(before.get("poses", 0)), "peer receives poses " + phase)
	return after

func enter() -> bool:
	app.menu.xr_button.pressed.emit()
	check(await wait_for(func(): return app.xr_lifecycle.state != "starting"), "entry completes")
	var entered: bool = app.xr_lifecycle.state == "xr"
	check(entered and app.xr and app.xr_viewport.use_xr and not root.use_xr, "enters real OpenXR session")
	if entered:
		var compositor := Compositor.new()
		compositor.compositor_effects = [render_probe]
		app.xr_camera.compositor = compositor
		check(await wait_for(func(): return app.xr_lifecycle.interface.get_session_state() == OpenXRInterface.SESSION_STATE_FOCUSED), "runtime focuses session")
		check(await wait_for(func(): return not app.align_xr_head), "tracked head anchors in room")
		check(await wait_for(func(): return app.sample_avatar_frame().tracked == 7), "head and both controllers track")
		var prior_frame: int = render_probe.snapshot().get("frame", 0)
		check(await wait_for(func(): return render_probe.snapshot().get("frame", 0) > prior_frame + 3), "fresh XR render frames")
		var rendered: Dictionary = render_probe.snapshot()
		print("XR_RENDER entry=", entries, " sample=", rendered)
		check(rendered.get("buffers", 0) == 2 and rendered.get("scene", 0) == 2, "render buffers and scene both stereo")
		check(float(rendered.get("ipd", 0)) > 0.04 and float(rendered.get("ipd", 0)) < 0.09, "rendered eye separation is physical IPD")
		await RenderingServer.frame_post_draw
		root.get_texture().get_image().save_png(output + "-vr-" + str(entries) + ".png")
		entries += 1
	return entered

func leave() -> void:
	var position: Vector3 = app.camera.global_position
	app.menu.xr_button.pressed.emit()
	check(await wait_for(func(): return app.xr_lifecycle.state == "desktop"), "leave completes")
	check(not app.xr and not app.xr_viewport.use_xr and not root.use_xr and not app.xr_lifecycle.interface.is_initialized(), "session fully released")
	check(app.camera.global_position.distance_to(position) < 0.01, "desktop inherits head position")
	check(XRServer.get_tracker("head") == null, "head tracker removed")
	var prior_frame: int = desktop_render_probe.snapshot().get("frame", 0)
	check(await wait_for(func(): return desktop_render_probe.snapshot().get("frame", 0) > prior_frame + 3), "fresh desktop render frames")
	var rendered: Dictionary = desktop_render_probe.snapshot()
	check(rendered.get("buffers", 0) == 1 and rendered.get("scene", 0) == 1, "render buffers and scene both mono on desktop")

func run() -> void:
	app = load("res://main.tscn").instantiate()
	root.add_child(app)
	render_probe = preload("res://tests/xr_render_probe.gd").new()
	var compositor := Compositor.new()
	desktop_render_probe = preload("res://tests/xr_render_probe.gd").new()
	compositor.compositor_effects = [desktop_render_probe]
	app.desktop_camera.compositor = compositor
	app.session.message_received.connect(receive)
	app.session.endpoint_ready.connect(func():
		if role == "host": FileAccess.open(output + ".endpoint", FileAccess.WRITE).store_string(app.session.get_endpoint_info()))
	app.toggle_connection()
	if not await wait_for(func(): return app.avatars.size() == 1):
		check(false, "peer connected"); finish(); return
	peer = app.avatars.keys()[0]
	app.select_device(OS.get_environment("PULSE_SOURCE"))
	app.set_muted(false)
	if role != "host":
		await create_timer(210).timeout
		if not finished: check(false, "host finished"); finish()
		return
	identities = persistent_nodes()
	endpoint = app.session.get_endpoint_info()
	app.playback.share_file(OS.get_environment("PRIM_TEST_HOST_FILE"))
	check(await wait_for(func(): return app.playback.loaded), "shared file loaded")
	await create_timer(2.0).timeout
	check(not app.xr and not app.xr_lifecycle.interface.is_initialized(), "desktop starts without runtime")
	app.menu.xr_button.pressed.emit()
	check(await wait_for(func(): return app.xr_lifecycle.state == "desktop"), "unavailable runtime falls back")
	check(app.session.is_active(), "failed entry keeps room")
	await signal_runner("runtime-start")
	var before := await probe("initial")
	for cycle in range(3):
		if not await enter(): finish(); return
		before = await continuity("vr-" + str(cycle), before)
		await leave()
		before = await continuity("desktop-" + str(cycle), before)
	if not await enter(): finish(); return
	await signal_runner("runtime-stop")
	check(await wait_for(func(): return app.xr_lifecycle.state == "desktop"), "runtime loss returns to desktop")
	before = await continuity("runtime-loss", before)
	await signal_runner("runtime-restart")
	if await enter():
		before = await continuity("runtime-recovered", before)
		await leave()
	app.session.broadcast_control(JSON.stringify({"type":"xr_finish"}))
	await create_timer(0.3).timeout
	if await enter(): finish(true)
	else: finish()

func finish(close_from_vr := false) -> void:
	if finished: return
	finished = true
	if app and role == "host":
		app.session.broadcast_control(JSON.stringify({"type":"xr_finish"}))
		await create_timer(0.2).timeout
	if app and app.xr_lifecycle.state != "desktop" and not close_from_vr:
		app.xr_lifecycle.stop()
		await wait_for(func(): return app.xr_lifecycle.state == "desktop")
	if app: app.set_muted(true)
	FileAccess.open(output + ".json", FileAccess.WRITE).store_string(JSON.stringify({"role":role, "failures":failures, "replies":replies}, "  "))
	print("XR_LIFECYCLE_RESULT ", failures)
	if close_from_vr:
		app.xr_lifecycle.changed.connect(func(state, _detail):
			if state == "desktop": FileAccess.open(output + ".closed-vr", FileAccess.WRITE).store_string("ok"))
		app.xr_lifecycle.close()
	else: quit(0 if failures.is_empty() else 1)
