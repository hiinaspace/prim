extends Node3D

var avatar_visible := false

var controller: XRController3D
var hand: int
var openxr_models := true
var fallback: MeshInstance3D
var models: Array[Node3D] = []

func _ready() -> void:
	fallback = MeshInstance3D.new()
	var mesh := BoxMesh.new()
	mesh.size = Vector3(0.07, 0.07, 0.12)
	fallback.mesh = mesh
	var material := StandardMaterial3D.new()
	material.albedo_color = Color("a6d8ca")
	fallback.material_override = material
	controller.add_child(fallback)
	if not openxr_models: return
	var manager := OpenXRRenderModelManager.new()
	manager.tracker = hand
	manager.render_model_added.connect(func(model): models.append(model))
	manager.render_model_removed.connect(func(model): models.erase(model))
	add_child(manager)

func _process(_delta: float) -> void:
	fallback.visible = not avatar_visible and controller.get_has_tracking_data() and not has_visible_model()

func has_visible_model() -> bool:
	for model in models:
		if is_instance_valid(model) and contains_visible_mesh(model): return true
	return false

func contains_visible_mesh(node: Node) -> bool:
	if node is MeshInstance3D and node.mesh != null and node.is_visible_in_tree(): return true
	for child in node.get_children():
		if contains_visible_mesh(child): return true
	return false

func set_avatar_visible(value: bool) -> void:
	avatar_visible = value
	visible = not value

func _exit_tree() -> void:
	if is_instance_valid(fallback): fallback.queue_free()
