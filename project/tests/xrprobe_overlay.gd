## xrprobe P1 scenario: Prim's companion overlay must draw the room's sky in
## both eyes. Generic xrprobe invariants only; no Prim-specific pixel oracle.
extends SceneTree
var app
var xrprobe: XRProbe

func _initialize() -> void: run.call_deferred()

func wait_for(predicate: Callable, seconds := 20.0) -> bool:
	var end := Time.get_ticks_msec() + int(seconds * 1000)
	while not predicate.call() and Time.get_ticks_msec() < end: await process_frame
	return predicate.call()

func run() -> void:
	xrprobe = XRProbe.new()
	root.add_child(xrprobe)
	app = load("res://main.tscn").instantiate()
	root.add_child(app)
	app.xr_lifecycle.start()
	xrprobe.check("prim entered xr", await wait_for(func(): return app.xr_lifecycle.state == "xr"))
	xrprobe.check("overlay session", app.xr_lifecycle.overlay_active)
	xrprobe.check("room primary", await wait_for(func(): return app.xr_lifecycle.room_primary))
	xrprobe.attach(app.xr_camera)
	await xrprobe.boot_check()
	for _i in 10: await process_frame
	var room := await xrprobe.capture("room")
	if room: xrprobe.check_capture(room, {"sky": true, "world": true})
	# Geometry culled: anything left can only be the environment's sky.
	var mask: int = app.xr_camera.cull_mask
	app.xr_camera.cull_mask = 0
	for _i in 3: await process_frame
	var sky := await xrprobe.capture("sky-only")
	if sky: xrprobe.check_capture(sky, {"sky": true})
	app.xr_camera.cull_mask = mask
	xrprobe.event("end", {"failures": xrprobe.failures})
	# Patched engine (0004): end the session through the app before quitting.
	app.xr_lifecycle.stop()
	await wait_for(func(): return app.xr_lifecycle.state == "desktop", 10)
	quit(1 if xrprobe.failures else 0)
