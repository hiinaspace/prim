extends Node3D

const AvatarDriver = preload("res://avatars/driver.gd")
const AvatarCatalog = preload("res://avatars/catalog.gd")
const HandPose = preload("res://avatars/hand_pose.gd")

const Menu = preload("res://ui/world_menu.gd")
const Avatar = preload("res://net/avatar.gd")
const Pose = preload("res://net/pose.gd")
const Playback = preload("res://media/playback.gd")
@onready var rig: XROrigin3D = $XROrigin3D
@onready var left: XRController3D = $XROrigin3D/LeftController
@onready var right: XRController3D = $XROrigin3D/RightController
@onready var player = $MPVPlayer
var visemes: Node
var local_avatar: PrimAvatarDriver
var avatar_id := "alicia"
var avatar_height := 1.6
var avatar_epoch := 0
var avatar_reset_epoch := 0
var grips: Array[XRController3D] = []
var calibration_remaining := 0.0
var calibration_samples: Array[float] = []
var camera: Camera3D
var menu: PrimMenu
var session
var sender
var playback
var avatars := {}
var settings := ConfigFile.new()
var xr := false
var xr_lifecycle: Node
var desktop_camera: Camera3D
@onready var xr_camera: XRCamera3D = $XROrigin3D/XRCamera3D
var xr_viewport: SubViewport
var reveal_effect: CompositorEffect
var reveal_gesture = preload("res://xr/reveal_gesture.gd").new()
var controller_neutral := false
var was_room_primary := false
var gesture_epoch := -1
var last_physical_head := Transform3D.IDENTITY

var audio_listener: Node3D
var xr_anchor: Transform3D
var align_xr_head := false
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
	avatar_id = settings.get_value("avatar", "id", "alicia")
	if not AvatarCatalog.MODELS.has(avatar_id): avatar_id = "alicia"
	avatar_height = clampf(settings.get_value("avatar", "eye_height", 1.6), 0.5, 2.5)
	rig.position = Vector3(0, 0, 3)
	desktop_camera = Camera3D.new()
	rig.add_child(desktop_camera)
	desktop_camera.position.y = 1.6
	camera = desktop_camera
	audio_listener = xr_camera.get_node("SteamAudioListener")
	audio_listener.reparent(camera, false)
	camera.make_current()
	# Startup opens the desktop menu; do not briefly grab another app's mouse.
	Input.mouse_mode = Input.MOUSE_MODE_VISIBLE
	for view in [desktop_camera, xr_camera]:
		view.cull_mask = 1 | AvatarDriver.LOCAL_FIRST | AvatarDriver.REMOTE
	for controller in [left,right]:
		var grip := XRController3D.new()
		grip.tracker = controller.tracker
		grip.pose = "grip"
		rig.add_child(grip)
		grips.append(grip)
	create_avatar_floor()
	movie_bus = AudioServer.get_bus_index("Movie")
	if movie_bus < 0:
		movie_bus = AudioServer.bus_count
		AudioServer.add_bus()
		AudioServer.set_bus_name(movie_bus, "Movie")
	var stereo_output := AudioStreamPlayer.new()
	stereo_output.name = "DirectStereo"
	stereo_output.bus = "Movie"
	add_child(stereo_output)
	$MPVPlayer.set_stereo_audio_target($MPVPlayer.get_path_to(stereo_output))
	$MPVPlayer.set_direct_stereo(bool(settings.get_value("audio", "direct_stereo", false)))
	for speaker in [$EmissiveScreen/LeftSpeaker, $EmissiveScreen/RightSpeaker]:
		speaker.bus = "Movie"
		speaker.attenuation_filter_db = 0.0
		speaker.air_absorption = false
		# Render each movie channel as a point source without first-order Ambisonics coloration.
		speaker.point_source_binaural = true
	session = ClassDB.instantiate("PrimSession")
	add_child(session)
	sender = ClassDB.instantiate("NetworkAudioSender")
	sender.capture_on_worker = true
	add_child(sender)
	sender.stop_capture()
	visemes = ClassDB.instantiate("PrimVisemes")
	add_child(visemes)
	visemes.attach_local(sender)
	sender.encoder_error.connect(func(message):
		status_text = "Microphone: " + message
		set_muted.call_deferred(true))
	menu = Menu.new()
	add_child(menu)
	menu.avatar_selected.connect(select_avatar)
	menu.height_changed.connect(set_avatar_height)
	menu.calibration_requested.connect(start_calibration)
	menu.set_avatar_settings(avatar_id, avatar_height)
	rebuild_local_avatar()
	menu.display_name.text = display_name
	menu.smooth.set_pressed_no_signal(smooth_turn)
	menu.speed.set_value_no_signal(turn_speed)
	menu.stereo.set_pressed_no_signal($MPVPlayer.is_direct_stereo())
	menu.movie_near.editable = not $MPVPlayer.is_direct_stereo()
	menu.movie_far.editable = not $MPVPlayer.is_direct_stereo()
	menu.stereo_changed.connect(func(value):
		$MPVPlayer.set_direct_stereo(value)
		menu.movie_near.editable = not value
		menu.movie_far.editable = not value
		save_setting("audio", "direct_stereo", value))
	menu.subtitle_selected.connect(func(id): $MPVPlayer.set_subtitle_track(id))
	$MPVPlayer.subtitle_tracks_changed.connect(func(): menu.update_subtitles($MPVPlayer.get_subtitle_tracks(), $MPVPlayer.get_subtitle_track()))
	get_window().files_dropped.connect(func(files): menu.accept_file_drop(files, camera))
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
	material.no_depth_test = true
	material.transparency = BaseMaterial3D.TRANSPARENCY_ALPHA
	material.render_priority = 101
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
	xr_viewport = SubViewport.new()
	xr_viewport.name = "HeadsetViewport"
	xr_viewport.world_3d = get_world_3d()
	xr_viewport.size = Vector2i(1280, 720)
	xr_viewport.render_target_update_mode = SubViewport.UPDATE_DISABLED
	xr_viewport.msaa_3d = get_viewport().msaa_3d
	add_child(xr_viewport)
	xr_lifecycle = preload("res://xr/session_lifecycle.gd").new()
	xr_lifecycle.render_viewport = xr_viewport
	add_child(xr_lifecycle)
	xr_lifecycle.mode_changed.connect(set_xr_mode)
	xr_lifecycle.presentation_changed.connect(update_xr_presentation)
	xr_lifecycle.changed.connect(menu.set_xr_state)
	menu.xr_toggled.connect(xr_lifecycle.toggle)
	menu.xr_hide_requested.connect(func():
		reveal_gesture.reset()
		xr_lifecycle.hide_reveal())
	menu.set_xr_state(xr_lifecycle.state, xr_lifecycle.detail)
	if xr_lifecycle.interface:
		xr_lifecycle.interface.pose_recentered.connect(on_xr_recentered)
	if "--desktop" not in OS.get_cmdline_user_args() and "--flat" not in OS.get_cmdline_user_args():
		xr_lifecycle.start.call_deferred()

func set_xr_mode(enabled: bool) -> void:
	if xr == enabled: return
	var head := camera.global_transform
	xr = enabled
	menu.set_vr_mode(enabled)
	if xr:
		xr_anchor = head
		align_xr_head = true
		desktop_camera.reparent(self, true)
		rig.reparent(xr_viewport, true)
		camera = xr_camera
		xr_viewport.render_target_update_mode = SubViewport.UPDATE_ALWAYS
		if xr_lifecycle.overlay_active:
			# Transparent viewports suppress Godot's sky. The reveal compositor
			# supplies alpha after drawing the full room, including its sky.
			xr_viewport.transparent_bg = false
			reveal_effect = preload("res://xr/reveal_effect.gd").new()
			var compositor := Compositor.new()
			compositor.compositor_effects = [reveal_effect]
			xr_camera.compositor = compositor
		menu.visible = false
		reveal_gesture.reset()
		controller_neutral = false
		for controller in [left, right]:
			var visuals := preload("res://xr/controller_visual.gd").new()
			visuals.controller = controller
			visuals.hand = OpenXRRenderModelManager.RENDER_MODEL_TRACKER_LEFT_HAND if controller == left else OpenXRRenderModelManager.RENDER_MODEL_TRACKER_RIGHT_HAND
			rig.add_child(visuals)
			controller_visuals.append(visuals)
		Input.mouse_mode = Input.MOUSE_MODE_VISIBLE
	else:
		align_xr_head = false
		xr_viewport.render_target_update_mode = SubViewport.UPDATE_DISABLED
		xr_viewport.transparent_bg = false
		xr_camera.compositor = null
		reveal_effect = null
		rig.reparent(self, true)
		desktop_camera.reparent(rig, true)
		camera = desktop_camera
		rig.global_transform = Transform3D(Basis(Vector3.UP, head.basis.get_euler().y), Vector3(head.origin.x, 0, head.origin.z))
		camera.position = Vector3(0, head.origin.y, 0)
		camera.rotation = Vector3(clampf(head.basis.get_euler().x, -1.45, 1.45), 0, 0)
		for visual in controller_visuals:
			visual.free()
		controller_visuals.clear()
		if get_window().has_focus(): capture_desktop_pointer()
		menu.xr_controls.text = "Desktop: WASD moves • mouse looks • Tab or Esc opens menu"
	camera.make_current()
	if xr: desktop_camera.make_current()
	audio_listener.reparent(camera, false)
	audio_listener.transform = Transform3D.IDENTITY
	avatar_configuration_changed()
	reset_avatar_motion("xr_mode_changed")
	if menu.visible: menu.open_at(camera)

func align_to_first_xr_pose() -> void:
	var tracker: XRPositionalTracker = XRServer.get_tracker("head")
	var pose: XRPose = tracker.get_pose("default") if tracker else null
	if not pose or not pose.has_tracking_data: return
	align_xr_head = false
	var angle := xr_anchor.basis.get_euler().y - camera.global_basis.get_euler().y
	rotate_about_head(angle)
	var displacement := xr_anchor.origin - camera.global_position
	displacement.y = 0
	rig.global_position += displacement
	reset_avatar_motion("xr_tracking_started")
	if menu.visible: menu.open_at(camera)

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
	set_muted(not muted)

func set_muted(value: bool) -> void:
	muted = value
	if muted:
		sender.stop_capture()
		if visemes: visemes.reset_source("local")
		if local_avatar: local_avatar.reset_visemes()
	else:
		if sender.has_method("set_input_gain_db"): sender.set_input_gain_db(menu.gain.value)
		sender.start_capture()
		muted = not sender.is_capturing()
	menu.set_microphone_muted(muted)

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
	visemes.attach_remote(peer, session.receive_stream(peer))
	avatars[peer] = avatar
	session.send_control(peer, JSON.stringify(local_avatar_state()))
	update_voice_volumes()
	playback.peer_joined(peer)

func peer_disconnected(peer: String) -> void:
	visemes.remove_source(peer)
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
	if message.get("type") == "avatar_state":
		if avatars.has(peer): avatars[peer].configure(message)
	elif message.get("type") == "name" and message.get("name") is String and avatars.has(peer):
		avatars[peer].nameplate.text = message.name.left(32)
	else:
		playback.message_received(peer, message)

func pose_received(peer: String, bytes: PackedByteArray) -> void:
	if not avatars.has(peer): return
	var pose := Pose.decode(bytes)
	if not pose.is_empty(): avatars[peer].apply_frame(pose)

func capture_desktop_pointer() -> void:
	if xr or camera == null or (menu != null and menu.visible): return
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
	if not xr and not menu.visible and event is InputEventMouseButton and event.button_index == MOUSE_BUTTON_LEFT and event.pressed and Input.mouse_mode != Input.MOUSE_MODE_CAPTURED:
		capture_desktop_pointer()
		get_viewport().set_input_as_handled()
		return
	if event is InputEventKey and event.pressed and not event.echo and event.physical_keycode == KEY_ESCAPE:
		toggle_menu(true)
		get_viewport().set_input_as_handled()
	elif event is InputEventKey and event.pressed and not event.echo and event.physical_keycode == KEY_TAB:
		toggle_menu(true)
		get_viewport().set_input_as_handled()
	elif menu.desktop_surface.visible and event is InputEventKey:
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

func toggle_menu(desktop := false) -> void:
	if menu.visible and (not xr or menu.desktop_in_vr == desktop):
		menu.visible = false
		var focus := menu.viewport.gui_get_focus_owner()
		if focus: focus.release_focus()
	else:
		if xr and desktop: menu.open_desktop()
		elif xr: menu.open_headset(camera)
		else: menu.open_at(camera)

func _process(delta: float) -> void:
	if camera == null: return
	if align_xr_head: align_to_first_xr_pose()
	if delta > 0.25: reset_avatar_motion("frame_stall")
	var allow_controllers := false
	if xr:
		var head_tracker: XRPositionalTracker = XRServer.get_tracker("head")
		var head_pose: XRPose = head_tracker.get_pose("default") if head_tracker else null
		var head_valid := head_pose != null and head_pose.has_tracking_data
		xr_lifecycle.set_head_valid(head_valid)
		if head_valid: last_physical_head = camera.global_transform
		if gesture_epoch != xr_lifecycle.gesture_epoch:
			gesture_epoch = xr_lifecycle.gesture_epoch
			reveal_gesture.reset()
		if not xr_lifecycle.room_primary:
			var amount: float = reveal_gesture.update(delta, camera.global_transform, head_valid,
				[grips[0].global_transform, grips[1].global_transform],
				[grips[0].get_has_tracking_data(), grips[1].get_has_tracking_data()],
				[grips[0].get_float("grip") > 0.65, grips[1].get_float("grip") > 0.65])
			xr_lifecycle.set_reveal_fraction(amount)
			for i in range(2):
				if reveal_gesture.haptic_amplitudes[i] > 0:
					grips[i].trigger_haptic_pulse("haptic", 0.0, reveal_gesture.haptic_amplitudes[i], 0.04, 0.0)
		if not head_valid: controller_neutral = false
		if head_valid and xr_lifecycle.room_primary and not controller_neutral:
			controller_neutral = controllers_are_neutral()
		allow_controllers = head_valid and xr_lifecycle.room_primary and controller_neutral
	var movement := Vector2.ZERO
	if xr and allow_controllers:
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
	if get_window().has_focus() and not menu.visible:
		movement += Vector2(float(Input.is_physical_key_pressed(KEY_D)) - float(Input.is_physical_key_pressed(KEY_A)), float(Input.is_physical_key_pressed(KEY_W)) - float(Input.is_physical_key_pressed(KEY_S))).limit_length()
	var forward := -camera.global_basis.z
	forward.y = 0
	var lateral := camera.global_basis.x
	lateral.y = 0
	move_body((forward.normalized() * movement.y + lateral.normalized() * movement.x) * 2.0 * delta)
	if xr: desktop_camera.global_transform = camera.global_transform
	update_video_volume()
	update_voice_volumes()
	var hand: XRController3D = right if right.get_has_tracking_data() else (left if left.get_has_tracking_data() else null)
	var origin := hand.global_position if xr and hand != null else camera.global_position
	var direction := -hand.global_basis.z if xr and hand != null else -camera.global_basis.z
	if not Input.is_mouse_button_pressed(MOUSE_BUTTON_LEFT): ignore_pointer_until_release = false
	var pressed := hand.get_float("trigger") > 0.6 if xr and hand != null else (not xr and Input.mouse_mode == Input.MOUSE_MODE_CAPTURED and not ignore_pointer_until_release and Input.is_mouse_button_pressed(MOUSE_BUTTON_LEFT))
	var target := menu.point(origin, direction, pressed) if xr and allow_controllers and menu.quad.visible else origin + direction * 5
	laser.visible = menu.quad.visible and xr and allow_controllers and hand != null
	pointer.visible = menu.quad.visible and xr and allow_controllers and hand != null
	pointer.global_position = target
	var mesh := laser.mesh as ImmediateMesh
	mesh.clear_surfaces()
	if laser.visible:
		mesh.surface_begin(Mesh.PRIMITIVE_LINES)
		mesh.surface_add_vertex(origin)
		mesh.surface_add_vertex(target)
		mesh.surface_end()
	var avatar_frame := sample_avatar_frame()
	var head_valid: bool = avatar_frame.tracked & 4 != 0
	if head_valid and not local_avatar.visible: local_avatar.ready_pose = false
	local_avatar.visible = head_valid
	local_avatar.apply_frame(avatar_frame.poses, avatar_frame.tracked, avatar_frame.fingers, avatar_frame.masks, avatar_frame.curls)
	local_avatar.apply_visemes(visemes.get_weights("local"), delta)
	for peer in avatars:
		if avatars[peer].body: avatars[peer].body.apply_visemes(visemes.get_weights(peer), delta)
	update_calibration(delta)
	var close_up: bool = menu.avatar_close_up.button_pressed
	var preview_target := camera.global_position - Vector3.UP * avatar_height * (0.035 if close_up else 0.45)
	var preview_forward := -camera.global_basis.z
	preview_forward.y = 0
	if preview_forward.length_squared() < 0.01: preview_forward = Vector3.FORWARD
	menu.avatar_camera.global_position = preview_target + preview_forward.normalized() * avatar_height * (0.4 if close_up else 1.8)
	menu.avatar_camera.look_at(preview_target)
	menu.avatar_preview.render_target_update_mode = SubViewport.UPDATE_WHEN_VISIBLE if menu.visible else SubViewport.UPDATE_DISABLED
	pose_elapsed += delta
	if session.is_active() and pose_elapsed >= 0.05:
		pose_elapsed = 0
		sequence = (sequence + 1) & 0xffffffff
		session.send_pose(Pose.encode(sequence, avatar_frame.poses, avatar_frame.tracked, avatar_frame.fingers, avatar_frame.masks, avatar_frame.curls, avatar_epoch, avatar_reset_epoch))
	menu.connection_button.text = "Disconnect" if session.is_active() else "Connect to friends"
	menu.avatar_mic_button.text = "Unmute microphone" if muted else "Mute microphone"
	menu.avatar_mic_status.text = "Speak to animate your avatar. Local preview only." if not session.is_active() else "Microphone audio is shared with this room when unmuted."
	var mic_status := "mic muted" if muted else ("MIC LIVE" if session.is_active() else "MIC LOCAL PREVIEW")
	menu.status.text = "%s • %d/6 people • %s" % [status_text, avatars.size() + 1, mic_status]
	if sender.has_method("get_input_peak_db"): menu.meter.value = sender.get_input_peak_db()

func rotate_about_head(angle: float) -> void:
	if not smooth_turn:
		avatar_reset_epoch = (avatar_reset_epoch + 1) & 0xffffffff
		if local_avatar: local_avatar.request_motion_reset("snap_turn")
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
		avatar.update_personal_space(camera.global_position)

func create_avatar_floor() -> void:
	var floor_body := StaticBody3D.new()
	floor_body.collision_layer = 2
	floor_body.collision_mask = 0
	floor_body.name = "AvatarGround"
	var collision := CollisionShape3D.new()
	var shape := BoxShape3D.new()
	shape.size = Vector3(20,0.2,20)
	collision.shape = shape
	floor_body.add_child(collision)
	floor_body.position.y = -0.1
	add_child(floor_body)

func local_avatar_state() -> Dictionary:
	return {"type":"avatar_state", "avatar":avatar_id, "revision":AvatarCatalog.REVISION, "eye_height":avatar_height if xr else 1.6, "epoch":avatar_epoch}

func rebuild_local_avatar() -> void:
	if local_avatar:
		remove_child(local_avatar)
		local_avatar.queue_free()
	local_avatar = AvatarDriver.new()
	add_child(local_avatar)
	local_avatar.configure(avatar_id, avatar_height if xr else 1.6, true)
	for visual in controller_visuals: visual.set_avatar_visible(true)

func select_avatar(id: String) -> void:
	if not AvatarCatalog.MODELS.has(id): return
	avatar_id = id
	save_setting("avatar", "id", id)
	avatar_configuration_changed()

func set_avatar_height(height: float) -> void:
	if not is_finite(height): return
	avatar_height = clampf(height, 0.5, 2.5)
	save_setting("avatar", "eye_height", avatar_height)
	avatar_configuration_changed()

func avatar_configuration_changed() -> void:
	avatar_epoch = (avatar_epoch + 1) & 0xffffffff
	rebuild_local_avatar()
	menu.set_avatar_settings(avatar_id, avatar_height)
	session.broadcast_control(JSON.stringify(local_avatar_state()))

func sample_avatar_frame() -> Dictionary:
	var poses: Array[Transform3D] = [camera.global_transform, Transform3D.IDENTITY, Transform3D.IDENTITY]
	var fingers: Array[Quaternion] = []
	var masks := PackedInt32Array([0,0])
	var curls := PackedFloat32Array()
	var tracked := 4
	if xr:
		var head_tracker: XRPositionalTracker = XRServer.get_tracker("head")
		var head_pose: XRPose = head_tracker.get_pose("default") if head_tracker else null
		if head_pose == null or not head_pose.has_tracking_data: tracked = 0
	for i in range(2):
		var tracker: XRHandTracker = XRServer.get_tracker("/user/hand_tracker/left" if i == 0 else "/user/hand_tracker/right") if xr else null
		var hand := HandPose.sample(tracker)
		fingers.append_array(hand.rotations)
		masks[i] = hand.mask
		curls.append_array(HandPose.curls(grips[i]) if xr else PackedFloat32Array([0,0,0,0,0]))
		if xr:
			var wrist := HandPose.wrist(rig, grips[i], tracker, i == 0)
			poses[i+1] = wrist.pose
			if wrist.valid: tracked |= 1 << i
	return {"poses":poses,"tracked":tracked,"fingers":fingers,"masks":masks,"curls":curls}

func start_calibration() -> void:
	if not xr:
		menu.avatar_status.text = "Height measurement uses the VR headset; desktop uses a 1.60 m viewpoint."
		return
	calibration_remaining = 3.5
	calibration_samples.clear()

func update_calibration(delta: float) -> void:
	if calibration_remaining <= 0: return
	calibration_remaining -= delta
	menu.avatar_status.text = "Stand upright, look forward… %.0f" % ceilf(calibration_remaining)
	if calibration_remaining < 0.5:
		var height := rig.to_local(camera.global_position).y
		if local_avatar.visible and height >= 0.5 and height <= 2.5: calibration_samples.append(height)
	if calibration_remaining <= 0:
		if calibration_samples.size() >= 3:
			calibration_samples.sort()
			set_avatar_height(calibration_samples[calibration_samples.size()/2])
			menu.avatar_status.text = "Standing eye height saved: %.2f m" % avatar_height
		else:
			menu.avatar_status.text = "Couldn't measure height. Check the tracking floor or adjust the height manually."

func reset_avatar_motion(reason: String = "recenter") -> void:
	avatar_reset_epoch = (avatar_reset_epoch + 1) & 0xffffffff
	if local_avatar: local_avatar.request_motion_reset(reason)

func controllers_are_neutral() -> bool:
	for controller in [left, right]:
		if not controller.get_has_tracking_data(): continue
		if controller.get_vector2("primary").length() > 0.2 or controller.get_float("trigger") > 0.2 or controller.get_float("grip") > 0.2: return false
		if controller.is_button_pressed("ax_button") or controller.is_button_pressed("by_button"): return false
	return true

func update_xr_presentation(primary: bool, amount: float) -> void:
	if xr_lifecycle.state != "xr":
		if reveal_effect: reveal_effect.set_amount(0.0)
		return
	if primary != was_room_primary:
		was_room_primary = primary
		controller_neutral = false
		reveal_gesture.reset()
		menu_ready = false
		mute_ready = false
		snap_ready = false
	menu.set_headset_input_allowed(primary)
	menu.xr_hide_button.visible = xr and xr_lifecycle.overlay_active and not primary
	menu.xr_hide_button.disabled = amount <= 0.001
	if reveal_effect: reveal_effect.set_amount(amount)
	if xr and not primary:
		laser.visible = false
		pointer.visible = false
		menu.xr_controls.text = "Game active: lift to peek • desktop Tab menu / WASD • game keeps controller input"

func on_xr_recentered() -> void:
	if xr:
		xr_anchor = last_physical_head
		align_xr_head = true
		reveal_gesture.reset()
	reset_avatar_motion("recenter")
