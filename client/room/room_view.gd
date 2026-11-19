extends Node3D
## Visual side of the loop room, rebuilt on hot reload. Reads the shared model that
## main.gd keeps (roster, bot poses); owns no network state, so reloading this script or
## theater.tscn never drops the daemon connection.
##
## What it draws: the world (room/theater.glb, placed by PrimWorld), a roster on a board above the screen (also a 2D label in
## desktop mode). Remote members' avatars are avatar/avatar_manager.gd's, not drawn here.

const PrimWorld := preload("res://world/prim_world.gd")
const WORLD_FILE := "res://room/theater.glb"

var world: Node3D # the loaded PRIM_world glTF (room/theater.glb)
var placement := {} # PrimWorld.place() result: screen, speakers, spawn
var model # main.gd's RoomModel
var board: Label3D
var hud: Label


func setup(m, desktop: bool) -> void:
	model = m
	var loaded := PrimWorld.load_file(WORLD_FILE)
	if loaded.root == null:
		push_error("room_view: world %s failed to load" % WORLD_FILE)
	else:
		world = loaded.root
		placement = loaded.placement
		add_child(world)
		for w in placement.warnings:
			push_warning("PRIM_world: %s" % w)
	board = Label3D.new()
	board.name = "RosterBoard"
	# above the screen (falls back to the old fixed spot without one)
	board.position = (placement.screen.pos + Vector3(0, 1.45, 0.1)) if placement.get("screen") != null \
		else Vector3(0, 2.6, -1.9)
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


## Where a joining member starts: the world's spawn node (identity at the origin if none).
func spawn_transform() -> Transform3D:
	var s = placement.get("spawn")
	return Transform3D(Basis(Vector3.UP, s.yaw), s.pos) if s != null else Transform3D.IDENTITY
