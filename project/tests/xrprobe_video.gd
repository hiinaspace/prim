## M0 oracle: the theater screen shows moving video in BOTH eyes of a real
## OpenXR session. Reads the eye buffers the headset would get (xrprobe's
## CompositorEffect), not the desktop texture, so a broken zero-copy handoff
## under multiview cannot pass. Set PRIM_MEDIA to a moving video.
## XRVIDEO_FAULT=null_texture clears the screen's textures: the check must FAIL.
extends SceneTree
var app
var xrprobe: XRProbe
var fault := OS.get_environment("XRVIDEO_FAULT")
# Window of the 64x64 eye thumbnail where the 2 m theater screen sits (found from the baseline contact sheet).
const X0 := 22
const X1 := 42
const Y0 := 29
const Y1 := 40

func _initialize() -> void: run.call_deferred()

func wait_for(predicate: Callable, seconds := 20.0) -> bool:
	var end := Time.get_ticks_msec() + int(seconds * 1000)
	while not predicate.call() and Time.get_ticks_msec() < end: await process_frame
	return predicate.call()

func window_stats(cells: PackedFloat32Array) -> Dictionary:
	var side := int(sqrt(cells.size() / 5))
	var lit := 0
	var n := 0
	var luma := 0.0
	for y in range(Y0, Y1):
		for x in range(X0, X1):
			var o := (y * side + x) * 5
			var l := 0.2126 * cells[o] + 0.7152 * cells[o + 1] + 0.0722 * cells[o + 2]
			luma += l
			# Video content is saturated (colour bars); the room around it is grey-blue.
			if maxf(cells[o], maxf(cells[o + 1], cells[o + 2])) - minf(cells[o], minf(cells[o + 1], cells[o + 2])) > 0.25: lit += 1
			n += 1
	return {"lit_frac": float(lit) / n, "mean_luma": luma / n}

func window_diff(a: PackedFloat32Array, b: PackedFloat32Array) -> float:
	var side := int(sqrt(a.size() / 5))
	var total := 0.0
	var n := 0
	for y in range(Y0, Y1):
		for x in range(X0, X1):
			var o := (y * side + x) * 5
			total += absf(a[o] - b[o]) + absf(a[o + 1] - b[o + 1]) + absf(a[o + 2] - b[o + 2])
			n += 1
	return total / n

func run() -> void:
	xrprobe = XRProbe.new()
	root.add_child(xrprobe)
	app = load("res://main.tscn").instantiate()
	root.add_child(app)
	app.xr_lifecycle.start()
	xrprobe.check("prim entered xr", await wait_for(func(): return app.xr_lifecycle.state == "xr"))
	await wait_for(func(): return not app.align_xr_head)
	xrprobe.attach(app.xr_camera)
	await xrprobe.boot_check()
	xrprobe.check("media loaded and playing", await wait_for(func(): return app.playback.loaded and app.player.is_playing(), 30))
	if fault == "null_texture":
		var material: StandardMaterial3D = app.get_node("EmissiveScreen").get_surface_override_material(0)
		material.albedo_texture = null
		material.emission_texture = null
		xrprobe.event("fault", {"injected": fault})
	await create_timer(1.0).timeout
	var caps := []
	for i in 4:
		await create_timer(0.7).timeout
		var rec := await xrprobe.capture("video-%d" % i)
		if rec: caps.append(rec)
	xrprobe.check("captured 4 frames", caps.size() == 4)
	for eye in 2:
		var name: String = ["left", "right"][eye]
		var stats := []
		for rec in caps: stats.append(window_stats(rec.eye_cells[eye]))
		var lit_ok := stats.size() == 4
		for s in stats: lit_ok = lit_ok and s.lit_frac > 0.4
		xrprobe.check("%s eye: screen window not black in every capture" % name, lit_ok, {"stats": stats})
		var changes := []
		for i in range(1, caps.size()):
			changes.append(window_diff(caps[i - 1].eye_cells[eye], caps[i].eye_cells[eye]))
		var moving := 0
		for c in changes: if c > 0.02: moving += 1
		xrprobe.check("%s eye: screen window changes between frames" % name, changes.size() == 3 and moving >= 2, {"diffs": changes})
	xrprobe.event("end", {"failures": xrprobe.failures})
	app.xr_lifecycle.stop()
	await wait_for(func(): return app.xr_lifecycle.state == "desktop", 10)
	quit(1 if xrprobe.failures else 0)
