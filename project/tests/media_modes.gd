extends SceneTree
# Two real players: room VOD, independent local playback, and RTMP -> RTSP live.
var app
var role := OS.get_environment("PRIM_TEST_ROLE")
var output := OS.get_environment("PRIM_TEST_OUTPUT")
var failures: Array[String] = []
var reports := {}
var done := false
func _initialize() -> void: call_deferred("run")
func check(ok: bool, label: String) -> void:
	print("MEDIA_CHECK ", label, ": ", ok)
	if not ok: failures.append(label)
func wait_for(predicate: Callable, seconds: float = 15) -> bool:
	var end := Time.get_ticks_msec() + seconds * 1000
	while not predicate.call() and Time.get_ticks_msec() < end: await process_frame
	return predicate.call()
func command(action: String) -> void:
	for peer in app.avatars:
		app.session.send_control(peer, JSON.stringify({"type":"media_test", "action":action}))
func receive(peer: String, raw: String) -> void:
	var msg: Variant = JSON.parse_string(raw)
	if not msg is Dictionary: return
	if msg.get("type") == "media_report": reports[peer] = msg
	if msg.get("type") != "media_test": return
	match msg.action:
		"local": app.playback.play_local_only(OS.get_environment("PRIM_TEST_HOST_FILE"))
		"return": app.playback.return_to_room()
		"pause_room": app.playback.request_action("toggle")
		"reconnect": app.playback.request_action("toggle")
		"report":
			app.session.send_control(peer, JSON.stringify({"type":"media_report", "source":app.playback.source, "local_only":app.playback.local_only, "loaded":app.playback.loaded, "live":app.playback.stream_mode(), "position":app.player.get_playback_position(), "paused":app.player.is_paused(), "state":app.player.get_playback_state(), "timeline":app.menu.timeline.editable, "speed":app.player.get_playback_state().speed}))
		"finish": finish()
func report() -> Dictionary:
	reports.clear()
	command("report")
	check(await wait_for(func(): return reports.size() == app.avatars.size()), "peers report playback")
	return reports.values()[0] if not reports.is_empty() else {}
func run() -> void:
	app = load("res://main.tscn").instantiate()
	root.add_child(app)
	app.set_process_input(false)
	app.session.message_received.connect(receive)
	app.session.endpoint_ready.connect(func():
		if role == "host": FileAccess.open(output + ".endpoint", FileAccess.WRITE).store_string(app.session.get_endpoint_info()))
	app.toggle_connection()
	check(await wait_for(func(): return app.avatars.size() >= 1, 40), "room connected")
	if role != "host":
		await create_timer(100).timeout
		if not done: check(false, "host completed"); finish()
		return
	app.playback.request_source(OS.get_environment("PRIM_TEST_MEDIA"))
	check(await wait_for(func(): return app.playback.loaded and app.player.get_playback_position() > 1), "finite HTTP video plays")
	await create_timer(1).timeout
	var state := await report()
	check(state.get("loaded", false) and not state.get("live", true) and state.get("timeline", false), "finite viewer keeps synchronized seek controls")
	var generation: int = app.playback.generation
	var room_source: String = app.playback.source
	app.menu.choose_source(OS.get_environment("PRIM_TEST_HOST_FILE"))
	check(app.menu.file_choice.visible and app.playback.generation == generation, "local selection prompts before changing playback")
	app.menu.local_file_requested.emit(app.menu.url.text)
	check(await wait_for(func(): return app.playback.loaded and app.player.get_playback_position() > 0.2), "host plays local file")
	check(app.playback.local_only and app.playback.source == room_source and app.playback.generation == generation, "host local file leaves room source and generation intact")
	app.playback.request_action("seek_to", 40)
	app.playback.request_action("toggle")
	await create_timer(1).timeout
	state = await report()
	check(not state.get("paused", true) and state.get("position", 100) < 25 and state.get("source") == room_source, "host local pause and seek do not reach viewer")
	command("pause_room")
	await create_timer(1).timeout
	check(app.playback.snapshot().paused, "viewer can pause room while host plays locally")
	app.playback.return_to_room()
	check(await wait_for(func(): return app.playback.loaded and app.player.is_paused()), "host returns to paused room")
	state = await report()
	check(absf(app.player.get_playback_position() - state.get("position", -100)) < 0.5, "host resumes room clock instead of local-file position")
	command("local")
	await create_timer(1).timeout
	state = await report()
	check(state.get("local_only", false) and state.get("loaded", false), "follower plays only locally")
	app.playback.request_action("seek_to", 30)
	await create_timer(1).timeout
	state = await report()
	check(state.get("local_only", false) and state.get("position", 100) < 15 and not state.get("paused", true), "room correction does not touch follower local file")
	command("return")
	await create_timer(2).timeout
	state = await report()
	check(not state.get("local_only", true) and state.get("paused", false) and absf(state.get("position", 0) - 30) < 0.5, "follower returns to latest room state")
	var live_url := OS.get_environment("PRIM_TEST_LIVE_URL")
	if not live_url.is_empty():
		app.playback.request_source(live_url)
		check(await wait_for(func(): return app.playback.loaded and app.player.get_playback_position() > 1, 20), "live stream loads and advances")
		await create_timer(2).timeout
		state = await report()
		print("LIVE_STATE ", JSON.stringify(state))
		check(app.playback.stream_mode() and state.get("live", false) and state.get("loaded", false), "host and viewer automatically recognize live stream")
		check(not app.menu.timeline.editable and not state.get("timeline", true), "live scrub controls disabled for both peers")
		var revision: int = app.playback.revision
		app.playback.apply_action("seek_to", 1000)
		app.playback.apply_action("toggle", 0)
		check(app.playback.revision == revision and not app.player.is_paused(), "room pause and seek requests ignored for live")
		check(state.get("speed") == 1.0 and not state.get("paused", true), "viewer stays at normal live playback speed")
		command("reconnect")
		await create_timer(3).timeout
		state = await report()
		check(app.playback.revision == revision and state.get("loaded", false), "viewer reconnects live without changing room intent")
		app.playback.request_source(OS.get_environment("PRIM_TEST_MEDIA"))
		check(await wait_for(func(): return app.playback.loaded and not app.playback.stream_mode()), "switch from live back to finite media")
		await create_timer(2).timeout
		state = await report()
		check(not state.get("live", true) and state.get("timeline", false), "finite sync controls return after live stream")
	command("finish")
	finish()
func finish() -> void:
	if done: return
	done = true
	FileAccess.open(output + ".json", FileAccess.WRITE).store_string(JSON.stringify({"role":role,"failures":failures}))
	print("MEDIA_MODES_RESULT ", JSON.stringify(failures))
	quit(0 if failures.is_empty() else 1)
