class_name PrimAvatar
extends Node3D

var head: Node3D
var left: Node3D
var right: Node3D
var voice: Node
var nameplate: Label3D
var targets: Array[Transform3D] = []
var last_sequence := -1
var last_update := 0

func setup(display_name: String, stream: AudioStream) -> void:
	head = box(Vector3(0.22,0.24,0.22), Color("83b8e3"))
	left = box(Vector3(0.09,0.07,0.16), Color("a6d8ca"))
	right = box(Vector3(0.09,0.07,0.16), Color("a6d8ca"))
	nameplate = Label3D.new()
	nameplate.text = display_name
	nameplate.font_size = 40
	nameplate.pixel_size = 0.004
	nameplate.billboard = BaseMaterial3D.BILLBOARD_ENABLED
	nameplate.position.y = 0.35
	head.add_child(nameplate)
	voice = ClassDB.instantiate("SteamAudioPlayer")
	voice.name = "Voice"
	voice.distance_attenuation = true
	voice.panning_strength = 0.0
	voice.attenuation_model = AudioStreamPlayer3D.ATTENUATION_DISABLED
	head.add_child(voice)
	voice.stream = stream
	voice.play()
	visible = false

func box(size: Vector3, color: Color) -> Node3D:
	var node := MeshInstance3D.new()
	var mesh := BoxMesh.new()
	mesh.size = size
	node.mesh = mesh
	var material := StandardMaterial3D.new()
	material.albedo_color = color
	node.material_override = material
	add_child(node)
	return node

func apply_pose(sequence: int, poses: Array[Transform3D], tracked: int) -> void:
	if sequence <= last_sequence or poses.size() != 3:
		return
	last_sequence = sequence
	last_update = Time.get_ticks_msec()
	targets = poses
	if not visible:
		head.global_transform = poses[0]
		left.global_transform = poses[1]
		right.global_transform = poses[2]
	visible = true
	left.visible = tracked & 1 != 0
	right.visible = tracked & 2 != 0

func _process(delta: float) -> void:
	if Time.get_ticks_msec() - last_update > 3000:
		visible = false
	if targets.size() == 3:
		var amount := 1 - exp(-delta * 20)
		head.global_transform = head.global_transform.interpolate_with(targets[0], amount)
		left.global_transform = left.global_transform.interpolate_with(targets[1], amount)
		right.global_transform = right.global_transform.interpolate_with(targets[2], amount)

func retire() -> void:
	if voice != null:
		voice.stop()
		voice.stream = null
	queue_free()
