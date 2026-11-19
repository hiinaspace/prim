extends GLTFDocumentExtension
## Reads the PRIM_world glTF extension (docs/PRIM_WORLD.md). Godot duplicates extension
## objects per import, so this is stateless: results are left on the scene as node meta
## ("prim_world" on marked nodes, "prim_world_root" on the root) and read by PrimWorld.

func _get_supported_extensions() -> PackedStringArray:
	return PackedStringArray(["PRIM_world"])


func _import_node(_state: GLTFState, _gltf_node: GLTFNode, json: Dictionary, node: Node) -> Error:
	var ext = json.get("extensions", {}).get("PRIM_world")
	if ext is Dictionary:
		node.set_meta("prim_world", ext)
	return OK


func _import_post(state: GLTFState, root: Node) -> Error:
	var ext = state.get_json().get("extensions", {}).get("PRIM_world")
	root.set_meta("prim_world_root", ext if ext is Dictionary else {})
	return OK
