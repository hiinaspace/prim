extends Node3D
## One remote member: head, left hand, right hand at the room-frame poses they publish in
## `member/<id>/pose` (PrimPose), drawn ~100 ms in the past and interpolated between the
## two samples around that moment, so 20 Hz updates look smooth. Nothing here touches the
## network; avatar_manager.gd feeds it decoded samples.
##
## Left and right are the person's own: facing them, their left hand is on your right.
## A hand whose tracked flag is off is hidden (desktop players have no hands).

const BUFFER_S := 0.10
const KEEP := 12

var key := ""
var head: Node3D
var left: Node3D
var right: Node3D
var label: Label3D
var samples: Array = [] # {t, head, left, right, flags}; t = arrival time, seconds
var last_rx := -1.0
var flags := 0
var fault_nointerp := OS.get_environment("PRIM_FAULT") == "nointerp"
var fault_mirror := OS.get_environment("PRIM_FAULT") == "mirror-x"


func setup(member_key: String, color: Color) -> void:
	key = member_key
	name = "Avatar_" + key.left(8)
	head = _part(SphereMesh.new(), color, 0.12)
	var nose := MeshInstance3D.new() # forward (-Z of the head) pointer: facing is readable
	nose.mesh = BoxMesh.new()
	nose.mesh.size = Vector3(0.05, 0.05, 0.16)
	nose.position = Vector3(0, 0, -0.14)
	nose.material_override = _mat(color)
	head.add_child(nose)
	label = Label3D.new()
	label.billboard = BaseMaterial3D.BILLBOARD_ENABLED
	label.no_depth_test = true
	label.pixel_size = 0.003
	label.font_size = 48
	label.outline_size = 10
	label.position = Vector3(0, 0.28, 0)
	head.add_child(label)
	left = _hand(Color(0.25, 0.5, 1.0)) # blue = left
	right = _hand(Color(1.0, 0.3, 0.25)) # red = right
	left.name = "LeftHand"
	right.name = "RightHand"
	head.name = "Head"
	for n in [head, left, right]:
		add_child(n)
	visible = false


func _mat(c: Color) -> StandardMaterial3D:
	var m := StandardMaterial3D.new()
	m.albedo_color = c
	m.emission_enabled = true
	m.emission = c * 0.5
	return m


func _part(mesh: Mesh, color: Color, radius: float) -> MeshInstance3D:
	var mi := MeshInstance3D.new()
	if mesh is SphereMesh:
		mesh.radius = radius
		mesh.height = radius * 2.0
	mi.mesh = mesh
	mi.material_override = _mat(color)
	return mi


func _hand(color: Color) -> Node3D:
	var h := _part(SphereMesh.new(), color, 0.05)
	var pointer := MeshInstance3D.new() # which way the hand points (-Z)
	pointer.mesh = BoxMesh.new()
	pointer.mesh.size = Vector3(0.02, 0.02, 0.1)
	pointer.position = Vector3(0, 0, -0.07)
	pointer.material_override = _mat(color)
	h.add_child(pointer)
	return h


func push(now: float, decoded: Dictionary) -> void:
	var s := {"t": now, "head": decoded["head"], "left": decoded["left"],
		"right": decoded["right"], "flags": decoded["flags"], "seq": decoded["seq"]}
	if fault_mirror:
		for part in ["head", "left", "right"]:
			s[part] = _mirror_x(s[part])
	samples.append(s)
	if samples.size() > KEEP:
		samples.pop_front()
	last_rx = now


## Mirror a pose in the x = 0 plane (the fault the handedness oracle must catch).
func _mirror_x(t: Transform3D) -> Transform3D:
	var m := Transform3D(Basis(Vector3(-1, 0, 0), Vector3(0, 1, 0), Vector3(0, 0, 1)), Vector3.ZERO)
	return m * t * m


func update(now: float) -> void:
	if samples.is_empty():
		return
	var at := now - BUFFER_S
	if fault_nointerp:
		at = INF
	var a: Dictionary = samples[0]
	var b: Dictionary = samples[samples.size() - 1]
	if at >= b["t"]:
		a = b
	else:
		for i in range(samples.size() - 1):
			if samples[i]["t"] <= at and at < samples[i + 1]["t"]:
				a = samples[i]
				b = samples[i + 1]
				break
		if at < samples[0]["t"]:
			b = a
	var w := 0.0
	if a != b:
		w = clampf((at - a["t"]) / maxf(b["t"] - a["t"], 1e-4), 0.0, 1.0)
	head.transform = (a["head"] as Transform3D).interpolate_with(b["head"], w)
	left.transform = (a["left"] as Transform3D).interpolate_with(b["left"], w)
	right.transform = (a["right"] as Transform3D).interpolate_with(b["right"], w)
	flags = a["flags"] if w < 0.5 else b["flags"]
	left.visible = flags & PrimPose.FLAG_LEFT != 0
	right.visible = flags & PrimPose.FLAG_RIGHT != 0


func set_label(text: String) -> void:
	if label.text != text:
		label.text = text
