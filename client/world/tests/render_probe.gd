extends SceneTree
## Renders a world from a fixed camera, saves a PNG, and probes the frame for the screen.
## Needs a real renderer (not --headless). Run:
##   godot --path client --script res://world/tests/render_probe.gd -- \
##       --world res://room/theater.glb --out /tmp/new.png [--drop-screen] [--report r.json]
##   --world res://room/theater.tscn renders the pre-glTF scene (no probe, no placement).
## --drop-screen frees the screen node after placement is read: the fault the probe must catch.
## Exit code 1 if a probe fails.

const PrimWorld := preload("res://world/prim_world.gd")
const CAM_POS := Vector3(0, 1.7, 3.5) # same as main.gd's desktop camera
const CAM_LOOK := Vector3(0, 1.1, -1.0)
const TOL := 6.0 / 255.0

var args := {}


func _init() -> void:
	var a := OS.get_cmdline_user_args()
	var i := 0
	while i < a.size():
		if a[i] in ["--world", "--out", "--report"]:
			args[a[i].substr(2)] = a[i + 1]
			i += 2
		else:
			args[a[i].substr(2)] = true
			i += 1
	root.size = Vector2i(1280, 720)
	var world: Node3D
	var placement := {}
	var path: String = args.get("world", "res://room/theater.glb")
	if path.ends_with(".tscn"):
		world = (load(path) as PackedScene).instantiate()
	else:
		var l := PrimWorld.load_file(path)
		world = l.root
		placement = l.placement
	root.add_child(world)
	if args.has("drop-screen") and placement.get("screen") != null:
		var n := world.get_node(placement.screen.path)
		n.get_parent().remove_child(n) # speakers are its children; the old scene lost them too
		n.free()
	var cam := Camera3D.new()
	root.add_child(cam)
	cam.position = CAM_POS
	cam.look_at(CAM_LOOK)
	cam.current = true
	await process_frame
	await process_frame
	await process_frame
	await RenderingServer.frame_post_draw
	var img := root.get_texture().get_image()
	img.save_png(args.get("out", "/tmp/render.png"))
	var rep := {"world": path, "size": [img.get_width(), img.get_height()], "checks": []}
	if placement.get("screen") != null:
		probe_screen(img, cam, placement.screen, rep)
	var fails := 0
	for c in rep.checks:
		print("%s  %s  %s" % ["PASS" if c.pass else "FAIL", c.name, c.detail])
		if not c.pass:
			fails += 1
	if args.has("report"):
		var f := FileAccess.open(args["report"], FileAccess.WRITE)
		f.store_string(JSON.stringify(rep, "  "))
	quit(1 if fails else 0)


func probe_screen(img: Image, cam: Camera3D, scr: Dictionary, rep: Dictionary) -> void:
	# Expected screen rectangle in pixels: project the quad's corners.
	var half: Vector2 = scr.size / 2.0
	var right := Vector3.RIGHT
	var up := Vector3.UP
	var lo := Vector2(1e9, 1e9)
	var hi := Vector2(-1e9, -1e9)
	for sx in [-1, 1]:
		for sy in [-1, 1]:
			var p: Vector2 = cam.unproject_position(scr.pos + right * half.x * sx + up * half.y * sy)
			lo = lo.min(p)
			hi = hi.max(p)
	var rect := Rect2i(Vector2i(lo.round()), Vector2i((hi - lo).round()))
	rep["expected_rect"] = [rect.position.x, rect.position.y, rect.size.x, rect.size.y]
	var inner := rect.grow(-3)
	var c := img.get_pixelv(rect.get_center())
	# (a) the rectangle interior is one flat colour (the screen's), (b) a ring just outside is not.
	var inside_n := 0
	var inside_ok := 0
	for y in range(inner.position.y, inner.end.y, 2):
		for x in range(inner.position.x, inner.end.x, 2):
			inside_n += 1
			if _near(img.get_pixel(x, y), c):
				inside_ok += 1
	var ring := rect.grow(6)
	var ring_n := 0
	var ring_diff := 0
	for x in range(ring.position.x, ring.end.x, 2):
		for y in [ring.position.y, ring.end.y - 1]:
			ring_n += 1
			if not _near(img.get_pixel(x, y), c):
				ring_diff += 1
	for y in range(ring.position.y, ring.end.y, 2):
		for x in [ring.position.x, ring.end.x - 1]:
			ring_n += 1
			if not _near(img.get_pixel(x, y), c):
				ring_diff += 1
	var frac_in := float(inside_ok) / maxf(inside_n, 1)
	var frac_out := float(ring_diff) / maxf(ring_n, 1)
	rep.checks.append({"name": "screen interior is one flat colour", "pass": frac_in > 0.97,
		"detail": "%.3f of %d samples near %s" % [frac_in, inside_n, c.to_html(false)]})
	rep.checks.append({"name": "ring outside the screen differs from it", "pass": frac_out > 0.9,
		"detail": "%.3f of %d samples differ" % [frac_out, ring_n]})
	rep["screen_color"] = c.to_html(false)


func _near(a: Color, b: Color) -> bool:
	return absf(a.r - b.r) < TOL and absf(a.g - b.g) < TOL and absf(a.b - b.b) < TOL
