extends SceneTree

var app
var role := OS.get_environment("PRIM_TEST_ROLE")
var peer := ""
var failures: Array[String] = []
var replies := {}
var output := OS.get_environment("PRIM_TEST_OUTPUT")
var done := false
var expected_peers := maxi(1, int(OS.get_environment("PRIM_TEST_PEERS")))

func _initialize() -> void:
	call_deferred("run")

func check(condition: bool, message: String) -> void:
	print("CHECK ", message, ": ", condition)
	if not condition: failures.append(message)

func wait_for(predicate: Callable, seconds: float = 15.0) -> bool:
	var deadline := Time.get_ticks_msec() + int(seconds * 1000)
	while not predicate.call() and Time.get_ticks_msec() < deadline:
		await process_frame
	return predicate.call()

func run() -> void:
	app = load("res://main.tscn").instantiate()
	root.add_child(app)
	check(not app.sender.is_capturing(), "microphone starts muted")
	app.session.message_received.connect(receive)
	app.session.endpoint_ready.connect(func():
		if role == "host":
			var file := FileAccess.open(output + ".endpoint", FileAccess.WRITE)
			file.store_string(app.session.get_endpoint_info()))
	app.toggle_connection()
	check(await wait_for(func(): return app.avatars.size() == expected_peers), "peer connected")
	if app.avatars.is_empty(): finish(); return
	peer = app.avatars.keys()[0]
	check(app.avatars.size() == expected_peers, "full mesh formed")
	app.select_device(OS.get_environment("PULSE_SOURCE"))
	app.set_muted(false)
	check(app.sender.is_capturing(), "virtual microphone starts")
	if role != "host":
		await create_timer(50.0).timeout
		if not done:
			check(false, "host completed test")
			finish()
		return
	app.playback.request_source(OS.get_environment("PRIM_TEST_MEDIA"))
	check(await wait_for(func(): return app.playback.loaded), "host media loaded")
	await create_timer(4.0).timeout
	await ask("playing")
	check(replies.get("playing", {}).get("loaded", false), "client media loaded")
	check(absf(float(replies.get("playing", {}).get("drift", 99))) < 0.4, "initial playback converges within 400 ms")
	app.playback.request_action("toggle")
	await create_timer(1.5).timeout
	await ask("paused")
	check(replies.get("paused", {}).get("paused", false), "pause reaches client")
	app.playback.request_action("seek", 12.0)
	await create_timer(2.0).timeout
	await ask("seek")
	check(absf(float(replies.get("seek", {}).get("position", -100)) - float(app.player.get_playback_position())) < 0.2, "paused seek within 200 ms")
	app.playback.request_action("toggle")
	await create_timer(2.0).timeout
	var before: int = app.sender.get_captured_input_frames()
	# This blocks only this process's Godot main thread; native capture/transport
	# and the receiving client's audio thread should continue.
	OS.delay_msec(700)
	var after: int = app.sender.get_captured_input_frames()
	check(after - before > 20000, "microphone capture survives 700 ms main-thread stall")
	await create_timer(2.0).timeout
	await ask("resumed")
	check(absf(float(replies.get("resumed", {}).get("drift", 99))) < 0.4, "resumed playback converges")
	var stream = app.session.receive_stream(peer)
	var stats: Dictionary = stream.get_stats()
	check(stats.get("non_silent_output_frames", 0) > 10000, "received voice decoded and mixed")
	check(replies.get("resumed", {}).get("voice_frames", 0) > 10000, "remote voice decoded and mixed")
	for remote in app.avatars:
		check(app.avatars[remote].last_sequence > 10, "remote poses applied")
		check(app.session.receive_stream(remote).get_stats().get("non_silent_output_frames", 0) > 10000, "each remote voice mixed")
	app.set_muted(true)
	var stopped: int = app.sender.get_captured_input_frames()
	await create_timer(0.5).timeout
	check(not app.sender.is_capturing() and app.sender.get_captured_input_frames() == stopped, "mute stops capture")
	root.get_texture().get_image().save_png(output + ".png")
	app.session.broadcast_control(JSON.stringify({"type":"test_finish"}))
	await create_timer(0.5).timeout
	app.toggle_connection()
	check(await wait_for(func(): return not app.session.is_active(), 5), "leave completes")
	check(app.muted and app.avatars.is_empty(), "leave clears avatars and mutes")
	finish()

func ask(phase: String) -> void:
	app.session.broadcast_control(JSON.stringify({"type":"test_probe", "phase":phase}))
	check(await wait_for(func(): return phase_complete(phase), 5), "reply " + phase)

func phase_complete(phase: String) -> bool:
	for remote in app.avatars:
		if not replies.has(phase + "/" + remote): return false
	return true

func receive(id: String, raw: String) -> void:
	var message: Variant = JSON.parse_string(raw)
	if not message is Dictionary: return
	if message.get("type") == "test_probe":
		var stream = app.session.receive_stream(id)
		var stats: Dictionary = stream.get_stats()
		app.session.send_control(id, JSON.stringify({"type":"test_reply", "phase":message.phase, "loaded":app.playback.loaded, "paused":app.player.is_paused(), "position":app.player.get_playback_position(), "drift":app.playback.drift_seconds, "voice_frames":stats.get("non_silent_output_frames", 0)}))
	elif message.get("type") == "test_reply":
		replies[message.phase] = message
		replies[message.phase + "/" + id] = message
		if message.phase in ["playing", "resumed"]:
			check(message.loaded and not message.paused and absf(float(message.drift)) < 0.4, "peer running playback converges")
	elif message.get("type") == "test_finish":
		app.set_muted(true)
		finish()

func finish() -> void:
	if done: return
	done = true
	var report := {"role":role, "failures":failures, "replies":replies}
	var file := FileAccess.open(output + ".json", FileAccess.WRITE)
	file.store_string(JSON.stringify(report, "  "))
	print("INTEGRATION_RESULT ", JSON.stringify(report))
	quit(0 if failures.is_empty() else 1)
