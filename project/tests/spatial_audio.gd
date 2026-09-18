extends SceneTree
var app
var output := OS.get_environment("PRIM_AUDIO_PROBE")
func _initialize(): call_deferred("run")
func run():
	app = load("res://main.tscn").instantiate()
	root.add_child(app)
	app.set_process_input(false)
	app.set_process(false)
	var speakers = [app.get_node("EmissiveScreen/LeftSpeaker"), app.get_node("EmissiveScreen/RightSpeaker")]
	for s in speakers:
		print("SETTINGS ", s.name, " ambi=",s.ambisonics," order=",s.ambisonics_order," air=",s.air_absorption," occ=",s.occlusion," refl=",s.reflection," directivity=",s.directivity," filter=",s.attenuation_filter_db)
	var channel_captures: Array[AudioEffectCapture] = []
	for i in range(2):
		var bus := AudioServer.bus_count
		AudioServer.add_bus()
		AudioServer.set_bus_name(bus, "ProbeChannel%d" % i)
		speakers[i].bus = AudioServer.get_bus_name(bus)
		var tap := AudioEffectCapture.new()
		tap.buffer_length = 4
		AudioServer.add_bus_effect(bus, tap)
		channel_captures.append(tap)
	var capture := AudioEffectCapture.new()
	capture.buffer_length = 4
	AudioServer.add_bus_effect(0,capture)
	app.playback.request_source(output.path_join("noise.mkv"))
	await create_timer(3).timeout
	for mode in ["direct", "legacy", "legacy_single", "bypass_single", "bypass_pair", "direct_again", "bypass_pair_again", "point", "point_single", "point_right", "point_left", "point_front", "point_turned"]:
		app.player.set_direct_stereo(mode.begins_with("direct"))
		for s in speakers:
			s.ambisonics = not mode.begins_with("bypass")
			s.point_source_binaural = mode.begins_with("point")
			s.volume_db = -6.0
		speakers[1].volume_db = -80.0 if mode.ends_with("single") or mode in ["point_right", "point_left", "point_front", "point_turned"] else -6.0
		if mode in ["point_right", "point_left", "point_front", "point_turned"]:
			var offset := Vector3(0, 0, -2)
			if mode == "point_right": offset = Vector3(2, 0, 0)
			if mode == "point_left": offset = Vector3(-2, 0, 0)
			speakers[0].global_position = app.camera.global_position + offset
		if mode == "point_turned": app.camera.rotation.y = PI / 2
		await create_timer(1).timeout
		AudioServer.lock()
		capture.clear_buffer()
		for tap in channel_captures: tap.clear_buffer()
		AudioServer.unlock()
		await create_timer(2).timeout
		var frames := capture.get_buffer(capture.get_frames_available())
		if mode.begins_with("bypass_pair"):
			for i in range(2):
				var tap := channel_captures[i]
				var separate := tap.get_buffer(tap.get_frames_available())
				var raw := FileAccess.open(output.path_join("%s-channel%d.raw" % [mode, i]), FileAccess.WRITE)
				for frame in separate: raw.store_float(frame.x)
				raw.close()
		var f := FileAccess.open(output.path_join(mode + ".f32"),FileAccess.WRITE)
		for frame in frames:
			f.store_float(frame.x)
			f.store_float(frame.y)
		f.close()
		print("CAPTURE ",mode," frames=",frames.size()," rate=",AudioServer.get_mix_rate())
	FileAccess.open(output.path_join("rate.json"), FileAccess.WRITE).store_string(JSON.stringify({"rate":AudioServer.get_mix_rate()}))
	quit()
