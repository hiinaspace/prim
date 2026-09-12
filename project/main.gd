extends Node3D

const Menu = preload("res://ui/world_menu.gd")
const Avatar = preload("res://net/avatar.gd")
const Pose = preload("res://net/pose.gd")
const Playback = preload("res://media/playback.gd")
@onready var rig: XROrigin3D = $XROrigin3D
@onready var left: XRController3D = $XROrigin3D/LeftController
@onready var right: XRController3D = $XROrigin3D/RightController
@onready var player = $MPVPlayer
var camera: Camera3D
var menu: PrimMenu
var session
var sender
var playback
var avatars := {}
var settings := ConfigFile.new()
var xr := false
var muted := true
var display_name := "Friend"
var smooth_turn := false
var turn_speed := 60.0
var snap_ready := true
var menu_ready := true
var mute_ready := true
var pose_elapsed := 0.0
var sequence := 0
var laser: MeshInstance3D
var pointer: MeshInstance3D
var controller_visuals: Array[Node3D] = []
const WALK_MIN := Vector2(-8.6, -3.65)
const WALK_MAX := Vector2(8.6, 6.95)
var movie_bus := -1
var ignore_pointer_until_release := false
var video_volume_db := 0.0
var movie_near_radius := 6.0
var movie_far_radius := 18.0
var voice_volume_percent := 150.0
var voice_near_radius := 3.0
var voice_far_radius := 15.0
var status_text := "Singleplayer"

func _ready() -> void:
	settings.load("user://settings.cfg")
	display_name = settings.get_value("user", "name", "Friend")
	smooth_turn = settings.get_value("comfort", "smooth_turn", false)
	turn_speed = settings.get_value("comfort", "turn_speed", 60.0)
	voice_volume_percent = clampf(settings.get_value("audio", "receive_volume", 150.0), 0, 300)
	voice_near_radius = clampf(settings.get_value("audio", "voice_near_radius", 3.0), 0.5, 12)
	voice_far_radius = clampf(settings.get_value("audio", "voice_far_radius", 15.0), voice_near_radius + 0.5, 40)
	movie_near_radius = clampf(settings.get_value("audio", "movie_near_radius", 6.0), 0.5, 12)
	movie_far_radius = clampf(settings.get_value("audio", "movie_far_radius", 18.0), movie_near_radius + 0.5, 40)
	rig.position = Vector3(0, 0, 3)
	var interface := XRServer.find_interface("OpenXR")
	if "--desktop" not in OS.get_cmdline_user_args() and "--flat" not in OS.get_cmdline_user_args() and interface:
		xr = interface.is_initialized() or interface.initialize()
	get_viewport().use_xr = xr
	if xr:
		camera = $XROrigin3D/XRCamera3D
		DisplayServer.window_set_vsync_mode(DisplayServer.VSYNC_DISABLED)
		for controller in [left, right]:
			var visuals := preload("res://xr/controller_visual.gd").new()
			visuals.controller = controller
			visuals.hand = OpenXRRenderModelManager.RENDER_MODEL_TRACKER_LEFT_HAND if controller == left else OpenXRRenderModelManager.RENDER_MODEL_TRACKER_RIGHT_HAND
			rig.add_child(visuals)
			controller_visuals.append(visuals)
	else:
		camera = Camera3D.new()
		rig.add_child(camera)
		camera.position.y = 1.6
		$XROrigin3D/XRCamera3D/SteamAudioListener.reparent(camera)
		camera.make_current()
		Input.mouse_mode = Input.MOUSE_MODE_CAPTURED
	movie_bus = AudioServer.get_bus_index("Movie")
	if movie_bus < 0:
		movie_bus = AudioServer.bus_count
		AudioServer.add_bus()
		AudioServer.set_bus_name(movie_bus, "Movie")
	for speaker in [$EmissiveScreen/LeftSpeaker, $EmissiveScreen/RightSpeaker]:
		speaker.bus = "Movie"
		speaker.attenuation_filter_db = 0.0
		speaker.air_absorption = false
	session = ClassDB.instantiate("PrimSession")
	add_child(session)
	sender = ClassDB.instantiate("NetworkAudioSender")
	sender.capture_on_worker = true
	add_child(sender)
	sender.stop_capture()
	sender.encoder_error.connect(func(message):
		status_text = "Microphone: " + message
		set_muted.call_deferred(true))
	menu = Menu.new()
	add_child(menu)
	menu.display_name.text = display_name
	menu.smooth.set_pressed_no_signal(smooth_turn)
	menu.speed.set_value_no_signal(turn_speed)
	menu.gain.set_value_no_signal(settings.get_value("audio", "gain", 0.0))
	AudioServer.input_device = settings.get_value("audio", "device", "Default")
	menu.refresh_devices()
	menu.set_voice_settings(voice_volume_percent, voice_near_radius, voice_far_radius)
	menu.set_movie_settings(movie_near_radius, movie_far_radius)
	menu.open_at(camera)
	playback = Playback.new()
	playback.player = player
	playback.session = session
	playback.menu = menu
	add_child(playback)
	menu.source_requested.connect(playback.request_source)
	menu.playback_toggled.connect(func(): playback.request_action("toggle"))
	menu.seek_requested.connect(func(seconds): playback.request_action("seek_to", seconds))
	menu.connection_toggled.connect(toggle_connection)
	menu.microphone_toggled.connect(toggle_microphone)
	menu.device_selected.connect(select_device)
	menu.gain_changed.connect(func(value):
		if sender.has_method("set_input_gain_db"): sender.set_input_gain_db(value)
		save_setting("audio", "gain", value))
	menu.voice_settings_changed.connect(set_voice_settings)
	menu.movie_settings_changed.connect(set_movie_settings)
	menu.name_changed.connect(func(value):
		display_name = value.strip_edges().left(32)
		if display_name.is_empty(): display_name = "Friend"
		save_setting("user", "name", display_name)
		session.broadcast_control(JSON.stringify({"type":"name", "name":display_name})))
	menu.smooth_turn_changed.connect(func(value):
		smooth_turn = value
		save_setting("comfort", "smooth_turn", value))
	menu.turn_speed_changed.connect(func(value):
		turn_speed = value
		save_setting("comfort", "turn_speed", value))
	menu.volume_changed.connect(func(value):
		video_volume_db = value
		update_video_volume())
	session.status_changed.connect(func(message): status_text = message)
	session.network_error.connect(func(message): status_text = message)
	session.host_changed.connect(func(_peer): playback.host_changed())
	session.peer_connected.connect(peer_connected)
	session.peer_disconnected.connect(peer_disconnected)
	session.left_room.connect(left_room)
	session.message_received.connect(message_received)
	session.pose_received.connect(pose_received)
	player.video_size_changed.connect(func(width, height):
		if width > 0 and height > 0:
			var size := Vector2(2.0 * width / height, 2.0)
			$EmissiveScreen.mesh.size = size
			$EmissiveScreen/AreaLight3D.area_size = size)
	laser = MeshInstance3D.new()
	laser.mesh = ImmediateMesh.new()
	var material := StandardMaterial3D.new()
	material.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
	material.albedo_color = Color("86e3cc")
	laser.material_override = material
	add_child(laser)
	pointer = MeshInstance3D.new()
	var sphere := SphereMesh.new()
	sphere.radius = 0.006
	sphere.height = 0.012
	pointer.mesh = sphere
	pointer.material_override = material
	add_child(pointer)
	var source := OS.get_environment("PRIM_MEDIA")
	if not source.is_empty(): playback.request_source(source)
	if OS.get_environment("PRIM_AUTOJOIN") == "1": toggle_connection()

func save_setting(section: String, key: String, value: Variant) -> void:
	settings.set_value(section, key, value)
	settings.save("user://settings.cfg")

func toggle_connection() -> void:
	if session.is_active():
		set_muted(true)
		session.leave_room()
		return
	var config: Variant = JSON.parse_string(FileAccess.get_file_as_string("res://private_lobby.json")) if FileAccess.file_exists("res://private_lobby.json") else null
	if not config is Dictionary or not config.get("secret", "") is String:
		status_text = "Private room configuration is missing from this build."
		return
	set_muted(true)
	var test_host := OS.get_environment("PRIM_TEST_HOST")
	if session.join_room(config.secret, display_name, test_host):
		session.attach_sender(sender)
		status_text = "Connecting…"

func toggle_microphone() -> void:
	if not session.is_active():
		status_text = "Connect to friends before enabling the microphone."
		return
	set_muted(not muted)

func set_muted(value: bool) -> void:
	muted = value
	if muted:
		sender.stop_capture()
	else:
		if sender.has_method("set_input_gain_db"): sender.set_input_gain_db(menu.gain.value)
		sender.start_capture()
		muted = not sender.is_capturing()
	menu.mic_button.text = "Unmute microphone" if muted else "Mute microphone"

func select_device(device: String) -> void:
	var was_muted := muted
	set_muted(true)
	AudioServer.input_device = device
	save_setting("audio", "device", device)
	if not was_muted: set_muted(false)

func peer_connected(peer: String, peer_name: String) -> void:
	if avatars.has(peer): return
	var avatar := Avatar.new()
	add_child(avatar)
	avatar.setup(peer_name, session.receive_stream(peer))
	avatars[peer] = avatar
	update_voice_volumes()
	playback.peer_joined(peer)

func peer_disconnected(peer: String) -> void:
	if avatars.has(peer):
		avatars[peer].retire()
		avatars.erase(peer)

func left_room() -> void:
	set_muted(true)
	for peer in avatars.keys(): peer_disconnected(peer)
	playback.left_room()
	status_text = "Singleplayer — disconnected"

func message_received(peer: String, json: String) -> void:
	var message: Variant = JSON.parse_string(json)
	if not message is Dictionary: return
	if message.get("type") == "name" and message.get("name") is String and avatars.has(peer):
		avatars[peer].nameplate.text = message.name.left(32)
	else:
		playback.message_received(peer, message)

func pose_received(peer: String, bytes: PackedByteArray) -> void:
	if not avatars.has(peer): return
	var pose := Pose.decode(bytes)
	if not pose.is_empty(): avatars[peer].apply_pose(pose.sequence, pose.poses, pose.tracked)

func capture_desktop_pointer() -> void:
	if xr or camera == null: return
	Input.mouse_mode = Input.MOUSE_MODE_CAPTURED
	ignore_pointer_until_release = true

func _notification(what: int) -> void:
	if xr or camera == null: return
	if what == NOTIFICATION_WM_WINDOW_FOCUS_IN:
		capture_desktop_pointer()
	elif what == NOTIFICATION_WM_WINDOW_FOCUS_OUT:
		Input.mouse_mode = Input.MOUSE_MODE_VISIBLE

func _input(event: InputEvent) -> void:
	if menu == null: return
	if not xr and event is InputEventMouseButton and event.button_index == MOUSE_BUTTON_LEFT and event.pressed and Input.mouse_mode != Input.MOUSE_MODE_CAPTURED:
		capture_desktop_pointer()
		get_viewport().set_input_as_handled()
		return
	if event is InputEventKey and event.pressed and not event.echo and event.physical_keycode == KEY_ESCAPE:
		if Input.mouse_mode == Input.MOUSE_MODE_CAPTURED: Input.mouse_mode = Input.MOUSE_MODE_VISIBLE
		else: capture_desktop_pointer()
		get_viewport().set_input_as_handled()
	elif event is InputEventKey and event.pressed and not event.echo and event.physical_keycode == KEY_TAB:
		toggle_menu()
		get_viewport().set_input_as_handled()
	elif menu.text_focused() and event is InputEventKey:
		menu.forward_key(event)
		get_viewport().set_input_as_handled()
	elif not xr and event is InputEventMouseMotion and Input.mouse_mode == Input.MOUSE_MODE_CAPTURED:
		rotate_about_head(-event.relative.x * 0.002)
		camera.rotation.x = clampf(camera.rotation.x - event.relative.y * 0.002, -1.45, 1.45)

static func locomotion_axes(left_active: bool, right_active: bool, left_stick: Vector2, right_stick: Vector2) -> Vector3:
	if left_active and right_active: return Vector3(left_stick.x, left_stick.y, right_stick.x)
	if left_active: return Vector3(0, left_stick.y, left_stick.x)
	if right_active: return Vector3(0, right_stick.y, right_stick.x)
	return Vector3.ZERO

func toggle_menu() -> void:
	if menu.visible:
		menu.visible = false
		var focus := menu.viewport.gui_get_focus_owner()
		if focus: focus.release_focus()
	else:
		menu.open_at(camera)

func _process(delta: float) -> void:
	if camera == null: return
	var movement := Vector2.ZERO
	if xr:
		var left_active := left.get_has_tracking_data()
		var right_active := right.get_has_tracking_data()
		var axes := locomotion_axes(left_active, right_active, left.get_vector2("primary"), right.get_vector2("primary"))
		movement = Vector2(axes.x, axes.y)
		if movement.length() < 0.2: movement = Vector2.ZERO
		var turn := axes.z
		menu.xr_controls.text = "One controller: stick moves + turns • A/X mic • trigger selects" if left_active != right_active else "VR: Y/B opens menu • A/X mic • trigger selects • sticks move / turn"
		if absf(turn) < 0.25: snap_ready = true
		elif smooth_turn: rotate_about_head(-turn * deg_to_rad(turn_speed) * delta)
		elif snap_ready:
			rotate_about_head(-signf(turn) * deg_to_rad(30))
			snap_ready = false
		var menu_down := (left_active and left.is_button_pressed("by_button")) or (right_active and right.is_button_pressed("by_button"))
		if menu_down and menu_ready: toggle_menu()
		menu_ready = not menu_down
		update_mute_button((left_active and left.is_button_pressed("ax_button")) or (right_active and right.is_button_pressed("ax_button")))
	elif not menu.text_focused():
		movement = Vector2(float(Input.is_physical_key_pressed(KEY_D)) - float(Input.is_physical_key_pressed(KEY_A)), float(Input.is_physical_key_pressed(KEY_W)) - float(Input.is_physical_key_pressed(KEY_S))).limit_length()
	var forward := -camera.global_basis.z
	forward.y = 0
	var lateral := camera.global_basis.x
	lateral.y = 0
	move_body((forward.normalized() * movement.y + lateral.normalized() * movement.x) * 2.0 * delta)
	update_video_volume()
	update_voice_volumes()
	var hand: XRController3D = right if right.get_has_tracking_data() else (left if left.get_has_tracking_data() else null)
	var origin := hand.global_position if xr and hand != null else camera.global_position
	var direction := -hand.global_basis.z if xr and hand != null else -camera.global_basis.z
	if not Input.is_mouse_button_pressed(MOUSE_BUTTON_LEFT): ignore_pointer_until_release = false
	var pressed := hand.get_float("trigger") > 0.6 if xr and hand != null else (not xr and Input.mouse_mode == Input.MOUSE_MODE_CAPTURED and not ignore_pointer_until_release and Input.is_mouse_button_pressed(MOUSE_BUTTON_LEFT))
	var target := menu.point(origin, direction, pressed)
	laser.visible = menu.visible and xr and hand != null
	pointer.visible = menu.visible and (not xr or hand != null)
	pointer.global_position = target
	var mesh := laser.mesh as ImmediateMesh
	mesh.clear_surfaces()
	if laser.visible:
		mesh.surface_begin(Mesh.PRIMITIVE_LINES)
		mesh.surface_add_vertex(origin)
		mesh.surface_add_vertex(target)
		mesh.surface_end()
	pose_elapsed += delta
	if session.is_active() and pose_elapsed >= 0.05:
		pose_elapsed = 0
		sequence += 1
		var poses: Array[Transform3D] = [camera.global_transform, left.global_transform, right.global_transform]
		var tracking := int(left.get_has_tracking_data()) | (int(right.get_has_tracking_data()) << 1) if xr else 0
		session.send_pose(Pose.encode(sequence, poses, tracking))
	menu.connection_button.text = "Disconnect" if session.is_active() else "Connect to friends"
	menu.status.text = "%s • %d/6 people • %s" % [status_text, avatars.size() + 1, "mic muted" if muted else "MIC LIVE"]
	if sender.has_method("get_input_peak_db"): menu.meter.value = sender.get_input_peak_db()

func rotate_about_head(angle: float) -> void:
	var pivot := camera.global_position
	var turn := Basis(Vector3.UP, angle)
	rig.global_transform = Transform3D(turn, pivot - turn * pivot) * rig.global_transform

func move_body(displacement: Vector3) -> void:
	if displacement.is_zero_approx(): return
	# Bounds apply to the body under the HMD, never to the tracking origin.
	# Clamping the origin after a turn undoes its room-scale pivot correction.
	var target := to_local(camera.global_position + displacement)
	target.x = clampf(target.x, WALK_MIN.x, WALK_MAX.x)
	target.z = clampf(target.z, WALK_MIN.y, WALK_MAX.y)
	var correction := to_global(target) - camera.global_position
	correction.y = 0
	rig.global_position += correction

func update_mute_button(pressed: bool) -> void:
	if pressed and mute_ready: toggle_microphone()
	mute_ready = not pressed

func update_video_volume() -> void:
	if movie_bus >= 0: AudioServer.set_bus_volume_db(movie_bus, video_volume_db)
	for speaker in [$EmissiveScreen/LeftSpeaker, $EmissiveScreen/RightSpeaker]:
		var distance: float = camera.global_position.distance_to(speaker.global_position)
		speaker.volume_linear = Avatar.voice_falloff(distance, movie_near_radius, movie_far_radius)

func set_movie_settings(near_radius: float, far_radius: float) -> void:
	movie_near_radius = clampf(near_radius, 0.5, 12)
	movie_far_radius = clampf(far_radius, movie_near_radius + 0.5, 40)
	menu.set_movie_settings(movie_near_radius, movie_far_radius)
	update_video_volume()
	settings.set_value("audio", "movie_near_radius", movie_near_radius)
	settings.set_value("audio", "movie_far_radius", movie_far_radius)
	settings.save("user://settings.cfg")

func set_voice_settings(percent: float, near_radius: float, far_radius: float) -> void:
	voice_volume_percent = clampf(percent, 0, 300)
	voice_near_radius = clampf(near_radius, 0.5, 12)
	voice_far_radius = clampf(far_radius, voice_near_radius + 0.5, 40)
	menu.set_voice_settings(voice_volume_percent, voice_near_radius, voice_far_radius)
	update_voice_volumes()
	settings.set_value("audio", "receive_volume", voice_volume_percent)
	settings.set_value("audio", "voice_near_radius", voice_near_radius)
	settings.set_value("audio", "voice_far_radius", voice_far_radius)
	settings.save("user://settings.cfg")

func update_voice_volumes() -> void:
	for avatar in avatars.values():
		avatar.update_voice_volume(camera.global_position, voice_volume_percent, voice_near_radius, voice_far_radius)
