## X0 oracle driver: can per-eye captures (VR) or the desktop window read the block-clock
## frame code off the theater screen, and when? One role per process.
## Env: X0_MODE=vr|desktop  X0_OUT=<dir>  X0_ROLE=<name>  X0_START_UNIX=<unix s to unpause>
##      X0_RECT="x,y,w,h" (fractions of the eye buffer)  X0_SECONDS=<capture window>
##      X0_BURST=<consecutive-frame captures first>  PRIM_MEDIA=<block-clock video>
extends SceneTree
const Crop := preload("res://tests/eye_crop_probe.gd")
var app
var xrprobe: XRProbe
var crop
var mode := OS.get_environment("X0_MODE") if OS.has_environment("X0_MODE") else "vr"
var out := OS.get_environment("X0_OUT")
var role := OS.get_environment("X0_ROLE") if OS.has_environment("X0_ROLE") else "a"
var log_f: FileAccess
var ticks0 := 0
var unix0 := 0.0

func _initialize() -> void: run.call_deferred()

func wait_for(predicate: Callable, seconds := 20.0) -> bool:
	var end := Time.get_ticks_msec() + int(seconds * 1000)
	while not predicate.call() and Time.get_ticks_msec() < end: await process_frame
	return predicate.call()

func unix_of(t_us: int) -> float:
	return unix0 + float(t_us - ticks0) / 1e6

func rec(d: Dictionary) -> void:
	log_f.store_line(JSON.stringify(d))
	log_f.flush()

func save_floats(arr: PackedFloat32Array, w: int, h: int, path: String) -> void:
	var img := Image.create_from_data(w, h, false, Image.FORMAT_RGBAF, arr.to_byte_array())
	img.convert(Image.FORMAT_RGBA8)
	img.save_png(path)

func grab(i: int, kind: String) -> void:
	if mode == "vr":
		var r: PackedStringArray = OS.get_environment("X0_RECT").split(",") if OS.has_environment("X0_RECT") else PackedStringArray(["0.3", "0.35", "0.4", "0.3"])
		crop.request(Rect2(float(r[0]), float(r[1]), float(r[2]), float(r[3])), "c%d" % i)
		var cap := {}
		for _n in 60:
			await process_frame
			cap = crop.take()
			if not cap.is_empty(): break
		if cap.is_empty():
			rec({"i": i, "kind": kind, "error": "no capture"}); return
		var files := []
		for e in cap.eyes.size():
			var f := "%s-%03d-%s.png" % [role, i, ["L", "R"][e]]
			save_floats(cap.eyes[e], cap.dim.x, cap.dim.y, out.path_join(f))
			files.append(f)
		rec({"i": i, "kind": kind, "t_unix": unix_of(cap.t_us), "t_done_unix": unix_of(cap.t_done_us), "render_frame": cap.frame,
			"files": files, "buf": [cap.size.x, cap.size.y], "org": [cap.org.x, cap.org.y], "dim": [cap.dim.x, cap.dim.y],
			"proc_frame": Engine.get_process_frames(), "error": cap.error})
	else:
		await RenderingServer.frame_post_draw
		var t := Time.get_ticks_usec()
		var img := root.get_texture().get_image()
		var f := "%s-%03d-D.png" % [role, i]
		img.save_png(out.path_join(f))
		rec({"i": i, "kind": kind, "t_unix": unix_of(t), "files": [f], "dim": [img.get_width(), img.get_height()], "proc_frame": Engine.get_process_frames()})

func run() -> void:
	DirAccess.make_dir_recursive_absolute(out)
	log_f = FileAccess.open(out.path_join("%s-caps.jsonl" % role), FileAccess.WRITE)
	ticks0 = Time.get_ticks_usec()
	unix0 = Time.get_unix_time_from_system()
	rec({"kind": "clock", "ticks0_us": ticks0, "unix0": unix0, "role": role, "mode": mode})
	app = load("res://main.tscn").instantiate()
	if mode == "vr":
		xrprobe = XRProbe.new()
		root.add_child(xrprobe)
	root.add_child(app)
	if mode == "vr":
		if OS.has_environment("X0_MULT"):
			var xi := XRServer.find_interface("OpenXR") as OpenXRInterface
			xi.render_target_size_multiplier = float(OS.get_environment("X0_MULT"))
		app.xr_lifecycle.start()
		await wait_for(func(): return app.xr_lifecycle.state == "xr")
		await wait_for(func(): return not app.align_xr_head)
		crop = Crop.new()
		var cam: Camera3D = app.xr_camera
		if cam.compositor == null: cam.compositor = Compositor.new()
		var fx := cam.compositor.compositor_effects
		fx.append(crop)
		cam.compositor.compositor_effects = fx
		xrprobe.attach(app.xr_camera) # existing 64x64 thumbnail path, for the size comparison
		await xrprobe.boot_check()
	else:
		app.set_process_input(false)
		app.menu.hide()
	rec({"kind": "ready", "mode": mode, "t_unix": unix_of(Time.get_ticks_usec())})
	var ok := await wait_for(func(): return app.playback.loaded and app.player.is_playing(), 40)
	rec({"kind": "playing", "ok": ok, "t_unix": unix_of(Time.get_ticks_usec())})
	# Pause on frame 0, then start at the agreed wall-clock instant.
	app.player.set_paused(true)
	app.player.seek(0.0)
	await create_timer(1.5).timeout
	app.player.set_paused(true)
	rec({"kind": "paused", "pos": app.player.get_playback_position(), "paused": app.player.is_paused()})
	await grab(0, "paused")
	if mode == "vr" and not OS.get_environment("X0_NOTHUMB") == "1":
		var th = await xrprobe.capture("thumb-paused")
		rec({"kind": "thumb", "files": th.get("eyes", []).map(func(e): return e.get("png", "")), "t_unix": unix_of(Time.get_ticks_usec())})
	var start := float(OS.get_environment("X0_START_UNIX")) if OS.has_environment("X0_START_UNIX") else 0.0
	while Time.get_unix_time_from_system() < start: await process_frame
	app.player.set_paused(false)
	rec({"kind": "unpause", "t_unix": Time.get_unix_time_from_system(), "target": start})
	var burst := int(OS.get_environment("X0_BURST")) if OS.has_environment("X0_BURST") else 30
	var seconds := float(OS.get_environment("X0_SECONDS")) if OS.has_environment("X0_SECONDS") else 12.0
	var t_end := Time.get_unix_time_from_system() + seconds
	var i := 1
	while i <= burst:
		await grab(i, "burst"); i += 1
	if mode == "vr" and OS.get_environment("X0_NOTHUMB") != "1":
		# The existing xrprobe path: 64x64 thumbnail of the whole eye buffer.
		for k in 12:
			var th = await xrprobe.capture("thumb-%02d" % k)
			var t_th := Time.get_ticks_usec()
			rec({"i": 1000 + k, "kind": "thumb", "t_unix": unix_of(t_th), "files": th.get("eyes", []).map(func(e): return "xrprobe/frames/" + e.get("png", "").get_file()),
				"size": th.get("size", Vector2i()), "dim": [64, 64]})
			await create_timer(0.1).timeout
	while Time.get_unix_time_from_system() < t_end:
		await grab(i, "sparse"); i += 1
		await create_timer(0.15).timeout
	rec({"kind": "pos", "pos": app.player.get_playback_position()})
	rec({"kind": "end"})
	if xrprobe: xrprobe.event("end", {"failures": xrprobe.failures})
	if mode == "vr":
		app.xr_lifecycle.stop()
		await wait_for(func(): return app.xr_lifecycle.state == "desktop", 10)
	quit(0)
