extends Node3D
## Visual side of the loop room, rebuilt on hot reload. Reads the shared model that
## main.gd keeps (roster, bot poses); owns no network state, so reloading this script or
## theater.tscn never drops the daemon connection.
##
## What it draws: the theater, a roster on a board above the screen (also a 2D label in
## desktop mode), and one marker per remote member that follows that member's
## `member/<id>/pose` live record.

const MARKER_COLORS := [Color(1.0, 0.45, 0.1), Color(0.2, 0.8, 1.0), Color(0.9, 0.2, 0.9)]

var model # main.gd's RoomModel
var board: Label3D
var markers := {} # member key -> Node3D
var hud: Label


func setup(m, desktop: bool) -> void:
	model = m
	var theater: Node = load("res://room/theater.tscn").instantiate()
	add_child(theater)
	board = Label3D.new()
	board.name = "RosterBoard"
	board.position = Vector3(0, 2.6, -1.9)
	board.pixel_size = 0.004
	board.font_size = 56
	board.outline_size = 12
	board.no_depth_test = true
	add_child(board)
	if desktop:
		var layer := CanvasLayer.new()
		hud = Label.new()
		hud.position = Vector2(16, 12)
		hud.add_theme_font_size_override("font_size", 22)
		hud.add_theme_color_override("font_outline_color", Color.BLACK)
		hud.add_theme_constant_override("outline_size", 6)
		layer.add_child(hud)
		add_child(layer)


func _process(_dt: float) -> void:
	if model == null:
		return
	var text: String = model.roster_text()
	board.text = text
	if hud:
		hud.text = text
	for key in model.poses:
		if key == model.me:
			continue
		var m: Node3D = markers.get(key)
		if m == null:
			m = _make_marker(markers.size())
			markers[key] = m
			add_child(m)
		var p: Dictionary = model.poses[key]
		m.position = p.pos
		m.rotation.y = p.yaw


func marker_position(key: String) -> Variant:
	var m: Node3D = markers.get(key)
	return m.position if m else null


func _make_marker(idx: int) -> Node3D:
	var root := Node3D.new()
	root.name = "Marker%d" % idx
	var mat := StandardMaterial3D.new()
	mat.albedo_color = MARKER_COLORS[idx % MARKER_COLORS.size()]
	mat.emission_enabled = true
	mat.emission = mat.albedo_color
	var body := MeshInstance3D.new()
	body.mesh = CapsuleMesh.new()
	body.mesh.radius = 0.2
	body.mesh.height = 1.2
	body.position.y = 0.8
	body.material_override = mat
	root.add_child(body)
	var head := MeshInstance3D.new()
	head.mesh = SphereMesh.new()
	head.mesh.radius = 0.13
	head.mesh.height = 0.26
	head.position.y = 1.55
	head.material_override = mat
	root.add_child(head)
	var nose := MeshInstance3D.new() # forward (-z) pointer so yaw is readable
	nose.mesh = BoxMesh.new()
	nose.mesh.size = Vector3(0.06, 0.06, 0.2)
	nose.position = Vector3(0, 1.55, -0.18)
	nose.material_override = mat
	root.add_child(nose)
	return root
