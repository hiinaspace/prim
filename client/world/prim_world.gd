extends RefCounted
## Loads a PRIM_world glTF (docs/PRIM_WORLD.md) and reports where the screen, speakers and
## spawn point are. `load_file` builds the scene; `place` is pure (scene in, placement out)
## so the golden test can run it on a fixture.

const TONEMAPS := {"linear": Environment.TONE_MAPPER_LINEAR, "reinhard": Environment.TONE_MAPPER_REINHARDT,
	"filmic": Environment.TONE_MAPPER_FILMIC, "aces": Environment.TONE_MAPPER_ACES, "agx": Environment.TONE_MAPPER_AGX}
const GROUP_SCREEN := "prim_screen"
const GROUP_SPEAKER := "prim_speaker"
const GROUP_SPAWN := "prim_spawn"

const Extension := preload("res://world/prim_world_extension.gd")
static var _ext: GLTFDocumentExtension


## Returns {root: Node3D, placement: Dictionary}; root is null and placement empty on failure.
static func load_file(path: String) -> Dictionary:
	if _ext == null:
		_ext = Extension.new()
		GLTFDocument.register_gltf_document_extension(_ext)
	var doc := GLTFDocument.new()
	var state := GLTFState.new()
	var err := doc.append_from_file(path, state)
	if err != OK:
		push_error("PrimWorld: cannot read %s (error %d)" % [path, err])
		return {"root": null, "placement": {}}
	var root: Node = doc.generate_scene(state)
	if root == null:
		return {"root": null, "placement": {}}
	var placement := place(root)
	_apply(root, placement)
	return {"root": root, "placement": placement}


## Placement dictionary, world space relative to `root`:
## {version, screen: {path, pos, basis_z, size}|null, speakers: [{path, channel, pos}],
##  spawn: {path, pos, yaw}|null, environment: {...}, warnings: [String]}
static func place(root: Node3D) -> Dictionary:
	var out := {"version": 0, "screen": null, "speakers": [], "spawn": null,
		"environment": {}, "warnings": []}
	var rootmeta: Dictionary = root.get_meta("prim_world_root", {})
	out.version = int(rootmeta.get("version", 0))
	out.environment = rootmeta.get("environment", {})
	if out.version != 1:
		out.warnings.append("PRIM_world version %s, expected 1" % out.version)
	_walk(root, root, Transform3D.IDENTITY, out)
	if out.screen == null:
		out.warnings.append("no screen node")
	if out.spawn == null:
		out.warnings.append("no spawn node")
	if (out.speakers as Array).size() < 2:
		out.warnings.append("fewer than two speakers")
	(out.speakers as Array).sort_custom(func(a, b): return str(a.channel) < str(b.channel))
	return out


static func _walk(root: Node3D, n: Node, xf: Transform3D, out: Dictionary) -> void:
	for c in n.get_children():
		if not c is Node3D:
			continue
		var cxf: Transform3D = xf * (c as Node3D).transform
		var m = c.get_meta("prim_world") if c.has_meta("prim_world") else null
		if m is Dictionary:
			var path := str(root.get_path_to(c))
			match str(m.get("role", "")):
				"screen":
					if out.screen != null:
						out.warnings.append("second screen %s ignored" % path)
					else:
						out.screen = {"path": path, "pos": cxf.origin, "normal": cxf.basis.z.normalized(),
							"size": Vector2(float(m.get("width", 0)), float(m.get("height", 0)))}
				"speaker":
					out.speakers.append({"path": path, "channel": str(m.get("channel", "")), "pos": cxf.origin})
				"spawn":
					if out.spawn != null:
						out.warnings.append("second spawn %s ignored" % path)
					else:
						# yaw of -Z (the facing direction), radians, 0 = looking down -Z
						out.spawn = {"path": path, "pos": cxf.origin, "yaw": cxf.basis.get_euler().y}
				var other:
					out.warnings.append("unknown role '%s' on %s" % [other, path])
		_walk(root, c, cxf, out)


## Applies loader policy to the scene: groups for roles, environment, directional shadows.
static func _apply(root: Node3D, placement: Dictionary) -> void:
	for key in ["screen", "spawn"]:
		if placement[key] != null:
			root.get_node(placement[key].path).add_to_group(GROUP_SCREEN if key == "screen" else GROUP_SPAWN)
	for s in placement.speakers:
		root.get_node(s.path).add_to_group(GROUP_SPEAKER)
	var env: Dictionary = placement.environment
	if not env.is_empty():
		var sky_mat := ProceduralSkyMaterial.new()
		sky_mat.sky_horizon_color = _color(env.get("sky_horizon"), sky_mat.sky_horizon_color)
		sky_mat.ground_horizon_color = _color(env.get("ground_horizon"), sky_mat.ground_horizon_color)
		var sky := Sky.new()
		sky.sky_material = sky_mat
		var e := Environment.new()
		e.background_mode = Environment.BG_SKY
		e.sky = sky
		e.tonemap_mode = TONEMAPS.get(str(env.get("tonemap", "linear")), Environment.TONE_MAPPER_LINEAR)
		e.glow_enabled = bool(env.get("glow", false))
		var we := WorldEnvironment.new()
		we.name = "WorldEnvironment"
		we.environment = e
		root.add_child(we)
	for l in root.find_children("*", "DirectionalLight3D", true, false):
		(l as DirectionalLight3D).shadow_enabled = true # glTF has no shadow flag


static func _color(v, fallback: Color) -> Color:
	if v is Array and v.size() >= 3:
		return Color(float(v[0]), float(v[1]), float(v[2]))
	return fallback
