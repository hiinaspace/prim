## Two local wrist panels; a single dwell arbiter prevents duplicate toggles.
extends Node3D
signal activated(action: String, hand: int)
const DWELL := 0.65
const SIZE := Vector2(0.17, 0.09)
var panels: Array[Node3D] = []
var labels: Array[Label3D] = []
var bars: Array[MeshInstance3D] = []
var target := -1
var elapsed := 0.0
var locked := false
var away := 0.0

func _ready() -> void:
	for hand in range(2):
		var panel := Node3D.new()
		add_child(panel)
		panels.append(panel)
		var backing := rectangle(SIZE, Color("172b38"))
		panel.add_child(backing)
		for action in range(2):
			var text := Label3D.new()
			text.font_size = 18
			text.pixel_size = 0.001
			text.no_depth_test = true
			text.render_priority = 120
			text.outline_render_priority = 119
			text.position = Vector3((-1 if action == 0 else 1) * 0.041, 0.005, 0.001)
			panel.add_child(text)
			labels.append(text)
			var bar := rectangle(Vector2(0.07, 0.006), Color("83e1c6"))
			bar.material_override.render_priority = 121
			bar.position = Vector3(text.position.x, -0.028, 0.002)
			panel.add_child(bar)
			bars.append(bar)
	visible = false

func rectangle(size: Vector2, color: Color) -> MeshInstance3D:
	var node := MeshInstance3D.new()
	var mesh := QuadMesh.new()
	mesh.size = size
	node.mesh = mesh
	var material := StandardMaterial3D.new()
	material.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
	material.albedo_color = color
	material.no_depth_test = true
	material.render_priority = 110
	node.material_override = material
	return node

func update_target(delta: float, next: int) -> int:
	if next < 0:
		away += delta
		if away >= 0.2: locked = false
	else: away = 0.0
	if next != target:
		target = next
		elapsed = 0.0
	if target < 0 or locked: return -1
	elapsed += minf(delta, 0.1) # A stalled frame must not activate a control.
	if elapsed < DWELL: return -1
	locked = true
	return target

func update_view(delta: float, enabled: bool, head: Transform3D, hands: Array, valid: Array, gesture_hand: int, muted: bool, deafened: bool) -> void:
	visible = enabled
	var nearest := INF
	var next := -1
	for hand in range(2):
		var panel := panels[hand]
		panel.visible = enabled and valid[hand]
		if panel.visible:
			panel.global_position = hands[hand] * Vector3(0, 0.035, 0.09)
			# Keep the anchor on the wrist, but present a readable face even when
			# the game/controller grip orientation points the palm elsewhere.
			if panel.global_position.distance_squared_to(head.origin) > 0.0001:
				var facing := (head.origin - panel.global_position).normalized()
				var up := head.basis.y if absf(facing.dot(head.basis.y)) < 0.99 else head.basis.x
				panel.look_at(head.origin, up, true)
			var local_head := panel.global_transform.affine_inverse() * head.origin
			var ray := panel.global_basis.inverse() * -head.basis.z
			if hand != gesture_hand and local_head.z > 0 and ray.z < -0.1:
				var distance := -local_head.z / ray.z
				var hit := local_head + ray * distance
				if distance > 0.1 and distance < 0.85 and absf(hit.x) < SIZE.x / 2 and absf(hit.y) < SIZE.y / 2 and distance < nearest:
					nearest = distance
					next = hand * 2 + (0 if hit.x < 0 else 1)
	var fired := update_target(delta, next)
	for hand in range(2):
		labels[hand * 2].text = "MUTED" if muted else "MIC ON"
		labels[hand * 2].modulate = Color("a6b7c7") if muted else Color("83e1c6")
		labels[hand * 2 + 1].text = "DEAF" if deafened else "LISTEN"
		labels[hand * 2 + 1].modulate = Color("ffbf75") if deafened else Color.WHITE
	for i in range(4):
		bars[i].scale.x = maxf(0.001, elapsed / DWELL) if i == target and not locked else 0.001
	if fired >= 0: activated.emit("mute" if fired % 2 == 0 else "deafen", fired / 2)
