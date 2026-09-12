class_name PrimAvatar
extends Node3D

var head: Node3D
var left: Node3D
var right: Node3D
var voice: Node
var nameplate: Label3D
var talking_indicator: Label3D
var receive_stream: AudioStream
var previous_voice_frames := 0
var talking_hold := 0.0
var talking := false
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
	talking_indicator = Label3D.new()
	talking_indicator.text = "● Speaking"
	talking_indicator.font_size = 28
	talking_indicator.pixel_size = 0.004
	talking_indicator.billboard = BaseMaterial3D.BILLBOARD_ENABLED
	talking_indicator.position.y = 0.50
	talking_indicator.modulate = Color("86e3cc")
	talking_indicator.visible = false
	head.add_child(talking_indicator)
	receive_stream = stream
	voice = ClassDB.instantiate("SteamAudioPlayer")
	voice.name = "Voice"
	voice.distance_attenuation = false
	voice.volume_linear = 0.0
	voice.ambisonics = true
	voice.occlusion = false
	voice.panning_strength = 0.0
	voice.attenuation_model = AudioStreamPlayer3D.ATTENUATION_DISABLED
	head.add_child(voice)
	# Preserve SteamAudioStream: assigning .stream here bypasses its HRTF wrapper.
	voice.play_stream(stream)
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
	if receive_stream != null:
		var frames: int = receive_stream.get_stats().get("non_silent_output_frames", 0)
		update_talking(frames > previous_voice_frames, delta)
		previous_voice_frames = frames
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

func update_voice_volume(listener: Vector3, percent: float, near_radius: float, far_radius: float) -> void:
	if voice == null: return
	var distance := head.global_position.distance_to(listener)
	voice.volume_linear = maxf(0.0, percent) / 100.0 * voice_falloff(distance, near_radius, far_radius)

static func voice_falloff(distance: float, near_radius: float, far_radius: float) -> float:
	# Full volume near the speaker, with a smooth fade to actual silence.
	# Keep decoding while inaudible so coming back into range resumes live audio.
	return 1.0 - smoothstep(near_radius, maxf(near_radius + 0.5, far_radius), distance)

func update_talking(active: bool, delta: float) -> void:
	# Hold across short speech gaps; activity is measured before local gain/falloff.
	talking_hold = 0.2 if active else maxf(0.0, talking_hold - delta)
	talking = talking_hold > 0.0
	talking_indicator.visible = talking
	nameplate.modulate = Color("86e3cc") if talking else Color.WHITE
