extends SceneTree
var app
var failures: Array[String] = []
func _initialize() -> void: call_deferred("run")
func check(value: bool, message: String) -> void:
	print("CHECK ", message, ": ", value)
	if not value: failures.append(message)
func spectrum(capture: AudioEffectCapture) -> Vector2:
	await create_timer(0.5).timeout
	capture.clear_buffer()
	await create_timer(0.3).timeout
	var frames := capture.get_buffer(capture.get_frames_available())
	var result := Vector2.ZERO
	for band in range(2):
		var frequency := 500.0 if band == 0 else 6000.0
		var real_part := 0.0
		var imaginary := 0.0
		for i in range(frames.size()):
			var phase := TAU * frequency * i / AudioServer.get_mix_rate()
			real_part += frames[i].x * cos(phase)
			imaginary += frames[i].x * sin(phase)
		result[band] = sqrt(real_part * real_part + imaginary * imaginary) / maxf(1, frames.size())
	return result
func run() -> void:
	app = load("res://main.tscn").instantiate()
	root.add_child(app)
	app.set_process(false)
	app.player.set_process(false)
	app.get_node("EmissiveScreen/RightSpeaker").stop()
	var speaker = app.get_node("EmissiveScreen/LeftSpeaker")
	speaker.global_position = app.camera.global_position + Vector3(0, 0, -2)
	var wav := AudioStreamWAV.new()
	wav.format = AudioStreamWAV.FORMAT_16_BITS
	wav.mix_rate = 48000
	wav.stereo = true
	wav.loop_mode = AudioStreamWAV.LOOP_FORWARD
	wav.loop_end = 48000
	var bytes := PackedByteArray()
	bytes.resize(48000 * 4)
	for i in range(48000):
		var sample := int(4000 * (sin(TAU * 500 * i / 48000.0) + sin(TAU * 6000 * i / 48000.0)))
		bytes.encode_s16(i * 4, sample)
		bytes.encode_s16(i * 4 + 2, sample)
	wav.data = bytes
	var capture := AudioEffectCapture.new()
	capture.buffer_length = 1.0
	# Capture downstream of the Movie bus fader.
	AudioServer.add_bus_effect(0, capture)
	speaker.play_stream(wav)
	speaker.stream_paused = false
	app.video_volume_db = 0
	app.update_video_volume()
	var loud: Vector2 = await spectrum(capture)
	app.video_volume_db = -18
	app.update_video_volume()
	var quiet: Vector2 = await spectrum(capture)
	var change := quiet / loud
	print("MOVIE_BAND_GAIN ", change)
	check(absf(change.x - db_to_linear(-18)) < 0.02 and absf(change.y - change.x) < 0.015, "movie slider changes low and high frequencies equally")
	app.video_volume_db = 0
	speaker.global_position = app.camera.global_position + Vector3(0, 0, -12)
	app.update_video_volume()
	var far: Vector2 = await spectrum(capture)
	var distance_change := far / loud
	print("MOVIE_DISTANCE_BAND_GAIN ", distance_change)
	check(absf(distance_change.x - 0.5) < 0.03 and absf(distance_change.y - distance_change.x) < 0.03, "distance attenuates both frequencies equally")
	speaker.stop()
	AudioServer.remove_bus_effect(0, 0)
	print("MOVIE_VOLUME_RESULT ", JSON.stringify(failures))
	quit(0 if failures.is_empty() else 1)
