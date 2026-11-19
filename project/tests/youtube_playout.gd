## M0 oracle: a URL starts through mpv's ytdl_hook (yt-dlp on PATH) to a first
## rendered frame, then plays to the end. Logs a timeline; records whether the
## end is a true end (position near duration) or an early stop/stall.
## Env: YT_URL, YT_OUT (dir), YT_MAX_SECONDS (default 900), YT_FIRST_FRAME_ONLY=1.
extends SceneTree
var app
var out := OS.get_environment("YT_OUT")
var finished_at := -1.0
var errors: Array[String] = []
var log_file: FileAccess

func _initialize() -> void: run.call_deferred()

func note(kind: String, data := {}) -> void:
	var rec := {"t": snappedf(Time.get_ticks_msec() / 1000.0, 0.1), "kind": kind}
	rec.merge(data)
	log_file.store_line(JSON.stringify(rec))
	log_file.flush()
	print("YT ", JSON.stringify(rec))

var video_view: SubViewport

func frame_stats() -> Dictionary:
	# Same readback surface as playback_controls.gd: the mpv output texture drawn
	# in a SubViewport (Texture2DRD itself has no CPU image).
	if video_view == null:
		video_view = SubViewport.new()
		video_view.size = Vector2i(640, 360)
		video_view.disable_3d = true
		video_view.render_target_update_mode = SubViewport.UPDATE_ALWAYS
		root.add_child(video_view)
		var rect := TextureRect.new()
		rect.texture = app.player.get_output_texture()
		rect.size = Vector2(640, 360)
		rect.expand_mode = TextureRect.EXPAND_IGNORE_SIZE
		video_view.add_child(rect)
	await create_timer(0.5).timeout
	await RenderingServer.frame_post_draw
	var image := video_view.get_texture().get_image()
	if image == null: return {"ok": false}
	var lit := 0
	var n := 0
	for y in range(0, image.get_height(), 8):
		for x in range(0, image.get_width(), 8):
			var c := image.get_pixel(x, y)
			if c.r + c.g + c.b > 0.06: lit += 1
			n += 1
	if not out.is_empty(): image.save_png(out.path_join("first-frame.png"))
	return {"ok": true, "lit_frac": float(lit) / maxf(n, 1)}

func run() -> void:
	DirAccess.make_dir_recursive_absolute(out)
	log_file = FileAccess.open(out.path_join("timeline.jsonl"), FileAccess.WRITE)
	var url := OS.get_environment("YT_URL")
	var max_s := float(OS.get_environment("YT_MAX_SECONDS") if not OS.get_environment("YT_MAX_SECONDS").is_empty() else "900")
	app = load("res://main.tscn").instantiate()
	root.add_child(app)
	app.set_process_input(false)
	app.player.playback_finished.connect(func():
		finished_at = app.player.get_playback_position()
		note("playback_finished", {"position": finished_at, "duration": app.player.get_duration()}))
	app.player.playback_error.connect(func(message):
		errors.append(str(message))
		note("playback_error", {"message": str(message)}))
	note("request", {"url": url})
	var started := Time.get_ticks_msec()
	app.playback.request_source(url)
	var first := false
	var last_pos := -1.0
	var last_move := Time.get_ticks_msec()
	var next_report := 0.0
	var verdict := "timeout"
	while true:
		await process_frame
		var elapsed := (Time.get_ticks_msec() - started) / 1000.0
		var pos: float = app.player.get_playback_position() if app.playback.loaded else 0.0
		if not first and app.playback.loaded and app.player.is_playing() and pos > 1.0:
			first = true
			var stats: Dictionary = await frame_stats()
			for _i in 20:
				if stats.get("ok", false) and stats.get("lit_frac", 0) > 0.05: break
				await create_timer(1.0).timeout
				stats = await frame_stats()
			note("first_frame", {"after_s": elapsed, "pos": pos, "frame": stats})
			if OS.get_environment("YT_FIRST_FRAME_ONLY") == "1":
				verdict = "first_frame" if stats.get("ok", false) and stats.get("lit_frac", 0) > 0.05 else "first_frame_black"
				break
		if first and absf(pos - last_pos) > 0.05:
			last_pos = pos
			last_move = Time.get_ticks_msec()
		if elapsed >= next_report:
			next_report = elapsed + 15.0
			note("tick", {"elapsed": elapsed, "pos": pos, "duration": app.player.get_duration(), "playing": app.player.is_playing(), "status": app.playback.status})
		if finished_at >= 0.0:
			var dur: float = app.player.get_duration()
			verdict = "true_end" if dur > 0 and finished_at >= dur - 3.0 else "early_end"
			break
		if not errors.is_empty() and not first:
			verdict = "error_before_first_frame"
			break
		if first and (Time.get_ticks_msec() - last_move) / 1000.0 > 45.0:
			verdict = "stalled"
			break
		if not first and elapsed > 120.0:
			verdict = "no_first_frame"
			break
		if elapsed > max_s:
			break
	note("verdict", {"verdict": verdict, "final_pos": last_pos, "duration": app.player.get_duration(), "errors": errors})
	log_file.close()
	quit(0 if verdict in ["first_frame", "true_end"] else 1)
