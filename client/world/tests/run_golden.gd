extends SceneTree
## Golden test: PRIM_world markup in (fixture.gltf) -> placement out (golden.json), then the
## same check on the real theater.glb. Run:
##   godot --headless --path client --script res://world/tests/run_golden.gd
## Exit code is the number of failed checks.

const PrimWorld := preload("res://world/prim_world.gd")
var fails := 0


func _init() -> void:
	var fx := PrimWorld.load_file("res://world/tests/fixture.gltf")
	check("fixture loads", fx.root != null)
	if fx.root != null:
		compare("fixture placement", summarize(fx.placement), JSON.parse_string(FileAccess.get_file_as_string("res://world/tests/golden.json")))
		check("Hidden node untouched by markup", not (fx.root as Node).find_child("Hidden", true, false).has_meta("prim_world"))
	var th := PrimWorld.load_file("res://room/theater.glb")
	check("theater.glb loads", th.root != null)
	if th.root != null:
		var p: Dictionary = th.placement
		check("theater: no warnings", p.warnings.is_empty(), str(p.warnings))
		check("theater screen at (0,1.1458,-2), 3.555x2", p.screen != null
			and (p.screen.pos as Vector3).distance_to(Vector3(0, 1.1457692, -2)) < 1e-4
			and (p.screen.size as Vector2).distance_to(Vector2(3.555, 2)) < 1e-4
			and (p.screen.normal as Vector3).distance_to(Vector3(0, 0, 1)) < 1e-4, str(p.screen))
		check("theater speakers at screen +-2 m", p.speakers.size() == 2
			and (p.speakers[0].pos as Vector3).distance_to(Vector3(-2, 1.1457692, -2)) < 1e-4
			and (p.speakers[1].pos as Vector3).distance_to(Vector3(2, 1.1457692, -2)) < 1e-4, str(p.speakers))
		check("theater spawn at origin facing -Z", p.spawn != null
			and (p.spawn.pos as Vector3).length() < 1e-4 and absf(p.spawn.yaw) < 1e-4, str(p.spawn))
		check("theater environment carried", p.environment.get("tonemap") == "filmic")
	print("golden: %d failure(s)" % fails)
	quit(fails)


func check(name: String, ok: bool, detail := "") -> void:
	print("%s  %s %s" % ["PASS" if ok else "FAIL", name, detail if not ok else ""])
	if not ok:
		fails += 1


func v3(v: Vector3) -> Array:
	return [snappedf(v.x, 1e-4), snappedf(v.y, 1e-4), snappedf(v.z, 1e-4)]


func summarize(p: Dictionary) -> Dictionary:
	var s := {"version": p.version, "environment": p.environment, "warnings": p.warnings,
		"screen": null, "spawn": null, "speakers": []}
	if p.screen != null:
		s.screen = {"path": p.screen.path, "pos": v3(p.screen.pos), "normal": v3(p.screen.normal),
			"size": [p.screen.size.x, p.screen.size.y]}
	if p.spawn != null:
		s.spawn = {"path": p.spawn.path, "pos": v3(p.spawn.pos), "yaw_deg": snappedf(rad_to_deg(p.spawn.yaw), 1e-2)}
	for sp in p.speakers:
		s.speakers.append({"path": sp.path, "channel": sp.channel, "pos": v3(sp.pos)})
	return s


func compare(name: String, got: Dictionary, want: Dictionary) -> void:
	# Round-trip both through JSON so ints/floats and Arrays compare alike.
	var g = JSON.parse_string(JSON.stringify(got))
	check(name, JSON.stringify(g, "", true) == JSON.stringify(want, "", true),
		"\n got  %s\n want %s" % [JSON.stringify(g, "", true), JSON.stringify(want, "", true)])
