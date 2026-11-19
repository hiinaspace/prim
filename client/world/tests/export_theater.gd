extends SceneTree
## One-shot exporter: client/room/theater.tscn (+ the old project's speakers and spawn) ->
## client/room/theater.glb with PRIM_world markup. Run:
##   godot --headless --path client --script res://world/tests/export_theater.gd
## Not part of the shipped client; kept so the export can be redone and audited.

class Exporter:
	extends GLTFDocumentExtension
	var root_ext := {}

	func _export_preflight(_state: GLTFState, root: Node) -> Error:
		root_ext = root.get_meta("prim_world_root", {})
		return OK

	func _export_node(state: GLTFState, _gltf_node: GLTFNode, json: Dictionary, node: Node) -> Error:
		if node.has_meta("prim_world"):
			state.add_used_extension("PRIM_world", false)
			var ex: Dictionary = json.get("extensions", {})
			ex["PRIM_world"] = node.get_meta("prim_world")
			json["extensions"] = ex
		return OK

	func _export_post(state: GLTFState) -> Error:
		var j := state.get_json()
		var ex: Dictionary = j.get("extensions", {})
		ex["PRIM_world"] = root_ext
		j["extensions"] = ex
		return OK


func _init() -> void:
	var out_path := "res://room/theater.glb"
	var root: Node3D = load("res://room/theater.tscn").instantiate()
	root.name = "Theater"
	# --- markup, from the old project/main.tscn ---------------------------------------
	var screen := root.get_node("EmissiveScreen") as MeshInstance3D
	var quad := screen.mesh as QuadMesh
	screen.set_meta("prim_world", {"role": "screen", "width": quad.size.x, "height": quad.size.y})
	for side in [["LeftSpeaker", "left", -2.0], ["RightSpeaker", "right", 2.0]]:
		var sp := Node3D.new() # SteamAudioPlayer in project/main.tscn, child of the screen
		sp.name = side[0]
		sp.position = Vector3(side[2], 0, 0)
		sp.set_meta("prim_world", {"role": "speaker", "channel": side[1]})
		screen.add_child(sp)
		sp.owner = root
	var spawn := Node3D.new() # project/main.tscn XROrigin3D at the origin, facing -Z
	spawn.name = "Spawn"
	spawn.set_meta("prim_world", {"role": "spawn"})
	root.add_child(spawn)
	spawn.owner = root
	var we := root.get_node("WorldEnvironment") as WorldEnvironment
	var sky := we.environment.sky.sky_material as ProceduralSkyMaterial
	root.set_meta("prim_world_root", {"version": 1, "environment": {
		"sky_horizon": [sky.sky_horizon_color.r, sky.sky_horizon_color.g, sky.sky_horizon_color.b],
		"ground_horizon": [sky.ground_horizon_color.r, sky.ground_horizon_color.g, sky.ground_horizon_color.b],
		"tonemap": ["linear", "reinhard", "filmic", "aces", "agx"][we.environment.tonemap_mode], "glow": we.environment.glow_enabled}})
	root.remove_child(we) # glTF cannot carry it; the markup does
	we.free()
	# --- export ------------------------------------------------------------------------
	var ext := Exporter.new()
	GLTFDocument.register_gltf_document_extension(ext)
	var doc := GLTFDocument.new()
	var state := GLTFState.new()
	var err := doc.append_from_scene(root, state)
	if err != OK:
		push_error("append_from_scene failed: %d" % err)
		quit(1)
		return
	err = doc.write_to_filesystem(state, out_path)
	print("export: ", error_string(err), " -> ", ProjectSettings.globalize_path(out_path))
	quit(0 if err == OK else 1)
