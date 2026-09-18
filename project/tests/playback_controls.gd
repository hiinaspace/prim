extends SceneTree
var app
var failures: Array[String] = []
var video_view: SubViewport
var video_rect: TextureRect
var directory := OS.get_environment("PRIM_TEST_CONTROLS_DIR")
func _initialize() -> void: call_deferred("run")
func check(ok: bool, label: String) -> void:
	print("CONTROL_CHECK ", label, ": ", ok)
	if not ok: failures.append(label)
func wait_for(predicate: Callable, seconds: float = 10) -> bool:
	var end := Time.get_ticks_msec() + seconds * 1000
	while not predicate.call() and Time.get_ticks_msec() < end: await process_frame
	return predicate.call()
func record(capture: AudioEffectCapture) -> PackedVector2Array:
	await create_timer(0.4).timeout
	capture.clear_buffer()
	await create_timer(0.2).timeout
	return capture.get_buffer(capture.get_frames_available())
func band(frames: PackedVector2Array, channel: int, hz: float) -> float:
	var value := Vector2.ZERO
	for i in range(frames.size()):
		var phase := TAU * hz * i / AudioServer.get_mix_rate()
		value += frames[i][channel] * Vector2(cos(phase), sin(phase))
	return value.length() / maxi(1, frames.size())
func subtitle_pixels() -> int:
	await create_timer(0.4).timeout
	await RenderingServer.frame_post_draw
	var picture: Image = video_view.get_texture().get_image()
	if picture == null: return -1
	var count := 0
	for y in range(picture.get_height() / 2, picture.get_height()):
		for x in range(picture.get_width()):
			if picture.get_pixel(x,y).r > 0.4: count += 1
	return count
func run() -> void:
	app = load("res://main.tscn").instantiate()
	root.add_child(app)
	app.set_process_input(false)
	video_view = SubViewport.new()
	video_view.size = Vector2i(640,360)
	video_view.disable_3d = true
	video_view.render_target_update_mode = SubViewport.UPDATE_ALWAYS
	root.add_child(video_view)
	video_rect = TextureRect.new()
	video_rect.texture = app.player.get_output_texture()
	video_rect.size = Vector2(640,360)
	video_view.add_child(video_rect)
	var capture := AudioEffectCapture.new()
	capture.buffer_length = 1
	AudioServer.add_bus_effect(0, capture)
	app.menu.stereo.button_pressed = true
	app.playback.request_source(directory.path_join("stereo.mkv"))
	check(await wait_for(func(): return app.playback.loaded and app.player.is_playing()), "stereo file loads")
	check(await wait_for(func(): return app.player.get_subtitle_tracks().size() == 2), "two embedded subtitle tracks")
	var frames: PackedVector2Array = await record(capture)
	var l440 := band(frames,0,440)
	var l880 := band(frames,0,880)
	var r440 := band(frames,1,440)
	var r880 := band(frames,1,880)
	print("STEREO_BANDS ", [l440,l880,r440,r880])
	check(l440 > 0.01 and r880 > 0.01 and l440 > l880 * 100 and r880 > r440 * 100, "direct stereo preserves left/right separation")
	check(not app.get_node("EmissiveScreen/LeftSpeaker").is_playing() and not app.get_node("EmissiveScreen/RightSpeaker").is_playing(), "spatial consumers stopped in direct mode")
	var generation: int = app.playback.generation
	for i in range(6):
		app.menu.stereo.button_pressed = i % 2 != 0
		await create_timer(0.1).timeout
	check(app.playback.generation == generation and app.playback.loaded, "switching output never reloads room media")
	check(app.player.set_subtitle_track(-1), "subtitles can be disabled")
	check(await wait_for(func(): return app.player.get_subtitle_track() == -1), "Off observed asynchronously")
	var off: int = await subtitle_pixels()
	print("SUBTITLE_OFF_PIXELS ", off)
	for track in app.player.get_subtitle_tracks():
		check(app.player.set_subtitle_track(track.id), "select embedded track")
		check(await wait_for(func(): return app.player.get_subtitle_track() == int(track.id)), "track selection observed")
		var pixels: int = await subtitle_pixels()
		print("SUBTITLE_ON_PIXELS ", pixels)
		video_view.get_texture().get_image().save_png(directory.path_join("subtitle-%s.png" % track.id))
		check(pixels > off + 100, "selected subtitle renders into video texture")
	app.playback.request_action("toggle")
	await create_timer(0.3).timeout
	app.player.set_subtitle_track(-1)
	check(await wait_for(func(): return app.player.get_subtitle_track() == -1), "subtitle switch while paused")
	check(await subtitle_pixels() == 0, "Off clears rendered subtitles while paused")
	app.menu.stereo.button_pressed = false
	app.menu.stereo.button_pressed = true
	check(app.player.is_paused(), "audio mode switch preserves pause")
	app.playback.request_action("seek_to", 5)
	app.playback.request_action("toggle")
	check(await wait_for(func(): return not app.player.is_paused() and app.player.get_playback_position() > 5), "seek/resume survives switches")
	app.video_volume_db = -12
	app.update_video_volume()
	frames = await record(capture)
	check(absf(band(frames,0,440)/l440 - db_to_linear(-12)) < 0.03, "movie volume applies to direct stereo")
	app.video_volume_db = 0
	app.update_video_volume()
	app.playback.request_source(directory.path_join("same.mkv"))
	check(await wait_for(func(): return app.playback.loaded and app.player.get_subtitle_tracks().is_empty()), "replacement clears subtitle menu")
	frames = await record(capture)
	var difference := 0.0
	var energy := 0.0
	for frame in frames:
		difference += (frame.x-frame.y)*(frame.x-frame.y)
		energy += frame.length_squared()
	check(energy > 0.1 and difference < energy * 0.00001, "identical channels stay sample-aligned")
	check(app.menu.subtitles.disabled, "no-subtitle selector disabled")
	app.playback.request_source(directory.path_join("surround.mkv"))
	check(await wait_for(func(): return app.playback.loaded), "surround file loads")
	frames = await record(capture)
	var center_left := band(frames,0,440)
	var center_right := band(frames,1,440)
	check(center_left > 0.01 and absf(center_left-center_right) < 0.001, "5.1 center downmix reaches both stereo ears")
	check(PrimMenu.normalize_file_path('"/tmp/a b.mkv"') == "/tmp/a b.mkv", "quoted path")
	check(PrimMenu.normalize_file_path("file:///tmp/a%20b.mkv") == "/tmp/a b.mkv", "local URI")
	check(PrimMenu.normalize_file_path("file://evil/path").is_empty(), "remote file URI rejected")
	check(PrimMenu.normalize_file_path("file:///bad%GG").is_empty(), "malformed URI rejected")
	var old_source: String = app.playback.source
	app.menu.connection_active = func(): return true
	app.menu.update_media_share(true, {})
	app.menu.accept_file_drop(PackedStringArray([directory.path_join("stereo.mkv")]), app.camera)
	check(app.playback.source == old_source and not app.menu.share_path.text.is_empty(), "multiplayer file drop prompts without replacing playback")
	app.playback.share_file("/does/not/exist")
	check(app.playback.source == old_source, "invalid share preserves playback")
	print("PLAYBACK_CONTROLS_RESULT ", JSON.stringify(failures))
	quit(0 if failures.is_empty() else 1)
