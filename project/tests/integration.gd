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

func wait_for(predicate: Callable, seconds: float = 40.0) -> bool:
	var deadline := Time.get_ticks_msec() + int(seconds * 1000)
	while not predicate.call() and Time.get_ticks_msec() < deadline:
		await process_frame
	return predicate.call()

func run() -> void:
	app = load("res://main.tscn").instantiate()
	root.add_child(app)
	check(not app.sender.is_capturing(), "microphone starts muted")
	check(await wait_for(func(): return app.visemes.get_status() != "loading", 10), "viseme initialization completes")
	check(app.visemes.get_status() == "ready", "viseme model available")
	if role == "host":
		app.select_device(OS.get_environment("PULSE_SOURCE"))
		app.menu.avatar_mic_button.pressed.emit()
		check(app.sender.is_capturing() and not app.session.is_active(), "avatar mirror enables local-only microphone")
		check(await wait_for(func(): return app.visemes.get_stats("local").get("hops", 0) > 35, 5), "singleplayer microphone drives visemes")
		check(app.local_avatar.expressions.owned.any(func(bind): return bind[0].get_blend_shape_value(bind[1]) > 0.001), "singleplayer mirror mouth animates")
		app.menu.avatar_close_up.button_pressed = true
		app.menu.avatar_picker.get_parent().get_parent().current_tab = 1
		await process_frame
		check(app.menu.avatar_camera.global_position.distance_to(app.camera.global_position) < 1.0, "mirror face close-up")
		await RenderingServer.frame_post_draw
		app.menu.viewport.get_texture().get_image().save_png(output + "-mirror.png")
		app.menu.avatar_mic_button.pressed.emit()
		check(app.muted and not app.sender.is_capturing() and app.visemes.get_weights("local").count(0.0) == 15, "mirror mute stops capture and clears mouth")
		app.menu.avatar_close_up.button_pressed = false
		app.menu.avatar_picker.get_parent().get_parent().current_tab = 0
		app.toggle_microphone()
		await create_timer(0.3).timeout
	app.session.message_received.connect(receive)
	app.session.status_changed.connect(func(value): print("NETWORK ", value))
	app.session.network_error.connect(func(value): print("NETWORK_ERROR ", value))
	app.session.endpoint_ready.connect(func():
		if role == "host":
			var file := FileAccess.open(output + ".endpoint", FileAccess.WRITE)
			file.store_string(app.session.get_endpoint_info()))
	if role != "host":
		app.playback.request_source(OS.get_environment("PRIM_TEST_MEDIA"))
		check(await wait_for(func(): return app.playback.loaded and not app.player.is_paused()), "client plays local media before joining")
	app.toggle_connection()
	check(app.muted and not app.sender.is_capturing(), "joining resets local preview to muted")
	check(await wait_for(func(): return app.avatars.size() == expected_peers), "peer connected")
	if app.avatars.is_empty(): finish(); return
	peer = app.avatars.keys()[0]
	check(app.avatars.size() == expected_peers, "full mesh formed")
	app.select_device("Default" if OS.get_name() == "Windows" else OS.get_environment("PULSE_SOURCE"))
	app.update_mute_button(true)
	app.update_mute_button(true)
	check(not app.muted, "held controller mute button toggles only once")
	app.update_mute_button(false)
	check(app.sender.is_capturing(), "virtual microphone starts")
	if role != "host":
		check(await wait_for(func(): return app.playback.source.is_empty() and not app.playback.loaded), "joining idle host clears local movie")
		check(app.menu.current_source.text.is_empty(), "idle host clears current source display")
		app.session.send_control(app.session.get_host_id(), JSON.stringify({"type":"test_idle_ready"}))
		await create_timer(50.0).timeout
		if not done:
			check(false, "host completed test")
			finish()
		return
	check(await wait_for(func(): return phase_complete("idle")), "all clients adopted idle host state")
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
	app.playback.request_action("seek_to", 15.0)
	await create_timer(2.0).timeout
	await ask("seek")
	check(absf(float(replies.get("seek", {}).get("position", -100)) - float(app.player.get_playback_position())) < 0.2, "paused seek within 200 ms")
	app.session.send_control(peer, JSON.stringify({"type":"test_scrub", "position":8.0}))
	await create_timer(1.5).timeout
	await ask("client_scrub")
	check(absf(app.player.get_playback_position() - 8.0) < 0.2 and absf(float(replies.get("client_scrub", {}).get("position", -100)) - 8.0) < 0.2, "client absolute scrub is ordered by host")
	app.playback.request_action("toggle")
	await create_timer(2.0).timeout
	var before: int = app.sender.get_captured_input_frames()
	# This blocks only this process's Godot main thread; native capture/transport
	# and the receiving client's audio thread should continue.
	var viseme_before: int = app.visemes.get_stats("local").get("hops", 0)
	OS.delay_msec(700)
	check(app.visemes.get_stats("local").get("hops", 0) - viseme_before > 30, "viseme analysis survives main-thread stall")
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
		check(app.visemes.get_stats(remote).get("hops", 0) > 100, "each remote viseme stream advances")
		check(app.session.receive_stream(remote).get_stats().get("non_silent_output_frames", 0) > 10000, "each remote voice mixed")
	check(app.avatars[peer].talking, "decoded remote audio lights talking indicator")
	await verify_avatar_replication()
	await verify_spatial_voice()
	app.update_mute_button(true)
	app.update_mute_button(false)
	var stopped: int = app.sender.get_captured_input_frames()
	await create_timer(0.5).timeout
	check(not app.sender.is_capturing() and app.sender.get_captured_input_frames() == stopped, "mute stops capture")
	check(app.visemes.get_weights("local").count(0.0) == 15, "mute clears local visemes")
	await ask("muted")
	check(replies.get("muted", {}).get("voice_paused", false), "mute drains to intentional silence")
	check(replies.get("muted", {}).get("mouth_closed", false), "mute closes remote mouth")
	var old_frames: float = replies.get("muted", {}).get("voice_frames", 0)
	app.set_muted(false)
	await create_timer(1.2).timeout
	await ask("unmuted")
	check(replies.get("unmuted", {}).get("voice_frames", 0) - old_frames > 20000, "unmute resumes fresh voice immediately")
	app.set_muted(true)
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
		app.session.send_control(id, JSON.stringify({"type":"test_reply", "phase":message.phase, "loaded":app.playback.loaded, "paused":app.player.is_paused(), "position":app.player.get_playback_position(), "drift":app.playback.drift_seconds, "voice_frames":stats.get("non_silent_output_frames", 0), "voice_paused":stats.get("playout_paused", false), "viseme_hops":app.visemes.get_stats(id).get("hops",0), "mouth_closed":app.visemes.get_weights(id).count(0.0)==15, "displayed_source":app.menu.current_source.text, "avatar":app.avatars[id].body.avatar_id if app.avatars[id].body else "", "voice_id":str(app.avatars[id].voice.get_instance_id()), "finger_mask":app.avatars[id].frame.get("masks",[0,0])[0], "finger_y":app.avatars[id].frame.get("fingers",[Quaternion.IDENTITY])[0].y}))
	elif message.get("type") == "test_idle_ready":
		replies["idle/" + id] = true
	elif message.get("type") == "test_reply":
		replies[message.phase] = message
		replies[message.phase + "/" + id] = message
		if message.phase in ["playing", "resumed"]:
			check(message.loaded and not message.paused and absf(float(message.drift)) < 0.4, "peer running playback converges")
			check(message.displayed_source == app.playback.source, "remote menu displays authoritative media source")
	elif message.get("type") == "test_scrub":
		app.playback.request_action("seek_to", message.position)
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

func voice_energy(capture: AudioEffectCapture) -> Vector2:
	await create_timer(0.4).timeout
	capture.clear_buffer()
	await create_timer(0.3).timeout
	var frames := capture.get_buffer(capture.get_frames_available())
	var energy := Vector2.ZERO
	for frame in frames:
		energy += Vector2(frame.x * frame.x, frame.y * frame.y)
	return energy / maxf(1, frames.size())

func verify_spatial_voice() -> void:
	var avatar = app.avatars[peer]
	var stream = app.session.receive_stream(peer)
	check(avatar.voice.get_inner_stream() == stream, "network voice retains Steam Audio processing wrapper")
	check(avatar.voice.get_parent() == avatar.head, "voice emitter follows remote head")
	var index := AudioServer.bus_count
	AudioServer.add_bus()
	AudioServer.set_bus_name(index, "SpatialVoiceTest")
	var capture := AudioEffectCapture.new()
	capture.buffer_length = 1.0
	AudioServer.add_bus_effect(index, capture)
	avatar.voice.bus = "SpatialVoiceTest"
	avatar.set_process(false)
	avatar.head.global_position = app.camera.global_position + Vector3(-2, 0, -1)
	var left_energy: Vector2 = await voice_energy(capture)
	avatar.head.global_position = app.camera.global_position + Vector3(2, 0, -1)
	var right_energy: Vector2 = await voice_energy(capture)
	print("SPATIAL_ENERGY ", left_energy, " / ", right_energy)
	check(left_energy.x > left_energy.y * 1.05 and right_energy.y > right_energy.x * 1.05, "Steam Audio moves decoded voice between ears with head position")
	var previous_volume: float = app.voice_volume_percent
	var previous_near: float = app.voice_near_radius
	var previous_far: float = app.voice_far_radius
	var mic_gain: float = app.menu.gain.value
	var movie_volume: float = app.video_volume_db
	app.set_voice_settings(previous_volume * 0.5, previous_near, previous_far)
	var quieter: Vector2 = await voice_energy(capture)
	var ratio := (quieter.x + quieter.y) / maxf(0.00000001, right_energy.x + right_energy.y)
	check(ratio > 0.15 and ratio < 0.35, "receive volume halves decoded voice amplitude")
	app.set_voice_settings(0, previous_near, previous_far)
	var before_frames: float = stream.get_stats().get("non_silent_output_frames", 0)
	var silent: Vector2 = await voice_energy(capture)
	check(silent.length_squared() < 0.000000000001, "zero receive volume silences output")
	check(stream.get_stats().get("non_silent_output_frames", 0) > before_frames + 10000, "receive mute keeps decoder advancing")
	for other in app.avatars.values():
		check(other.voice.volume_linear == 0, "receive volume applies to every remote voice")
	check(app.menu.gain.value == mic_gain and app.video_volume_db == movie_volume, "receive controls leave microphone and movie levels alone")
	app.set_voice_settings(previous_volume, 3, 6)
	avatar.head.global_position = app.camera.global_position + Vector3(7, 0, 0)
	var distant: Vector2 = await voice_energy(capture)
	check(distant.length_squared() < 0.000000000001, "outer voice radius silences distant speaker")
	avatar.head.global_position = app.camera.global_position + Vector3(2, 0, -1)
	var returned: Vector2 = await voice_energy(capture)
	check(returned.x + returned.y > (right_energy.x + right_energy.y) * 0.7, "returning inside inner radius restores live full-volume voice")
	app.set_voice_settings(previous_volume, previous_near, previous_far)
	avatar.voice.bus = "Master"
	avatar.set_process(true)
	AudioServer.remove_bus(index)

func verify_avatar_replication() -> void:
	await ask("avatar_before")
	app.set_process(false)
	app.select_avatar("vita")
	var sample: Dictionary = app.sample_avatar_frame()
	var joint := Quaternion(Vector3.UP, 0.4) * Quaternion(Vector3.RIGHT, 0.3)
	sample.fingers[0] = joint
	for tick in range(6):
		app.sequence = (app.sequence + 1) & 0xffffffff
		app.session.send_pose(app.Pose.encode(app.sequence, sample.poses, 4, sample.fingers, PackedInt32Array([32767,32767]), sample.curls, app.avatar_epoch, app.avatar_reset_epoch))
		await create_timer(0.05).timeout
	await ask("avatar_after")
	for remote in app.avatars:
		var before: Dictionary = replies.get("avatar_before/" + remote, {})
		var after: Dictionary = replies.get("avatar_after/" + remote, {})
		check(after.get("avatar") == "vita", "bundled avatar change reaches peer")
		check(after.get("finger_mask") == 32767 and absf(after.get("finger_y",0.0) - joint.y) < 0.00001, "articulated finger rotation reaches peer")
		check(before.get("voice_id") == after.get("voice_id") and after.get("voice_frames",0) > before.get("voice_frames",0), "avatar swap preserves advancing voice stream")
		check(app.avatars[remote].body != null, "remote body loaded")
		check(not app.avatars[remote].body.visible, "overlapping remote body is hidden from local camera")
	app.select_avatar("alicia")
	app.set_process(true)
