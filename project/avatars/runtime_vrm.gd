extends RefCounted

# Offline local-file proof. No peer downloads or editor-imported resource cache.
# Extension registration pattern checked against FPSloppa b0fc725 visual_loader.gd.
const Driver = preload("res://avatars/driver.gd")
const MAX_BYTES := 32 * 1024 * 1024
const REQUIRED := ["Hips", "Spine", "Head", "LeftUpperArm", "LeftLowerArm", "LeftHand", "RightUpperArm", "RightLowerArm", "RightHand", "LeftUpperLeg", "LeftLowerLeg", "LeftFoot", "RightUpperLeg", "RightLowerLeg", "RightFoot"]

# Keep a live template: PackedScene serialization of runtime script-typed arrays
# can lose their script identity in exported builds (.gd/.gdc remapping).
class ImportedAvatar extends RefCounted:
	var template: Node3D
	func instantiate() -> Node3D:
		return template.duplicate(Node.DUPLICATE_SCRIPTS) as Node3D
	func _notification(what: int) -> void:
		if what == NOTIFICATION_PREDELETE and is_instance_valid(template): template.free()

static func load_scene(path: String) -> Dictionary:
	var file := FileAccess.open(path, FileAccess.READ)
	if file == null: return {"error":"Cannot open the VRM file."}
	var size := file.get_length()
	if size < 28 or size > MAX_BYTES: return {"error":"Choose a self-contained VRM smaller than 32 MiB."}
	# Read once; validate exactly the bytes passed to GLTFDocument.
	var bytes := file.get_buffer(size)
	if bytes.size() != size: return {"error":"The VRM changed while reading it."}
	if bytes.decode_u32(0) != 0x46546c67 or bytes.decode_u32(4) != 2 or bytes.decode_u32(8) != size:
		return {"error":"Invalid binary VRM header."}
	var json_size := bytes.decode_u32(12)
	if bytes.decode_u32(16) != 0x4e4f534a or json_size > 4 * 1024 * 1024 or json_size + 28 > size:
		return {"error":"Invalid VRM metadata chunk."}
	var document: Variant = JSON.parse_string(bytes.slice(20, 20 + json_size).get_string_from_utf8())
	if not document is Dictionary or not document.get("extensions") is Dictionary:
		return {"error":"VRM metadata is missing."}
	var extensions: Dictionary = document.extensions
	if not extensions.has("VRM") and not extensions.has("VRMC_vrm"):
		return {"error":"A VRM 0.x or 1.0 humanoid is required."}
	var binary_offset := 20 + json_size
	if bytes.decode_u32(binary_offset + 4) != 0x004e4942 or binary_offset + 8 + bytes.decode_u32(binary_offset) != size:
		return {"error":"A self-contained binary VRM is required."}
	if contains_uri(document): return {"error":"External and data-URI references are not supported; embed assets in the VRM."}
	for key in ["nodes", "buffers", "bufferViews", "accessors", "images", "meshes", "materials", "skins"]:
		if not document.get(key, []) is Array or document.get(key, []).any(func(entry): return not entry is Dictionary):
			return {"error":"Invalid VRM structure: " + key}
	if document.get("nodes", []).size() > 4096 or document.get("images", []).size() > 64 or document.get("materials", []).size() > 128:
		return {"error":"This model exceeds local preview complexity limits."}
	var gltf := GLTFDocument.new()
	var plugins: Array[GLTFDocumentExtension] = [
		preload("res://addons/vrm/vrm_extension.gd").new(),
		preload("res://addons/vrm/1.0/VRMC_node_constraint.gd").new(),
		preload("res://addons/vrm/1.0/VRMC_springBone.gd").new(),
		preload("res://addons/vrm/1.0/VRMC_materials_mtoon.gd").new(),
		preload("res://addons/vrm/1.0/VRMC_materials_hdr_emissiveMultiplier.gd").new(),
		preload("res://addons/vrm/1.0/VRMC_vrm.gd").new(),
	]
	for plugin in plugins: GLTFDocument.register_gltf_document_extension(plugin, true)
	var state := GLTFState.new()
	state.handle_binary_image = GLTFState.HANDLE_BINARY_EMBED_AS_UNCOMPRESSED
	state.set_additional_data("vrm/head_hiding_method", 3)
	state.set_additional_data("vrm/first_person_layers", 2)
	state.set_additional_data("vrm/third_person_layers", 4)
	# IMPORT_USE_NAMED_SKIN_BINDS = 16 in Godot 4.7; no editor-only API dependency.
	var error := gltf.append_from_buffer(bytes, "", state, 16)
	var model: Node3D = gltf.generate_scene(state) if error == OK else null
	for plugin in plugins: GLTFDocument.unregister_gltf_document_extension(plugin)
	if model == null: return {"error":"The VRM importer could not load this model."}
	var skeleton := Driver.find_skeleton(model)
	if skeleton == null:
		model.free()
		return {"error":"The VRM has no supported humanoid skeleton."}
	for bone in REQUIRED:
		if skeleton.find_bone(bone) < 0:
			model.free()
			return {"error":"Missing required humanoid bone: " + bone}
	var head := skeleton.get_bone_global_rest(skeleton.find_bone("Head"))
	if not head.is_finite() or head.origin.y < 0.1:
		model.free()
		return {"error":"Unsupported humanoid height or rest pose."}
	var scene := ImportedAvatar.new()
	scene.template = model
	return {"scene":scene, "version":"1.0" if extensions.has("VRMC_vrm") else "0.x"}

static func contains_uri(value: Variant) -> bool:
	if value is Dictionary:
		if value.has("uri"): return true
		for child in value.values():
			if contains_uri(child): return true
	elif value is Array:
		for child in value:
			if contains_uri(child): return true
	return false
