class_name PrimMenu
extends Node3D

signal avatar_selected(id: String)
signal height_changed(height: float)
signal xr_hide_requested
signal xr_toggled
signal xr_open_requested
signal xr_takeover_confirmed
signal calibration_requested

signal stereo_changed(enabled: bool)
signal subtitle_selected(id: int)

signal source_requested(source: String)
signal local_file_requested(path: String)
signal room_playback_requested
signal share_file_requested(path: String)
signal stop_share_requested
signal relay_decided(media_id: String, peer: String, allow: bool)
signal media_limit_changed(mbps: int)
signal playback_toggled
signal seek_requested(seconds: float)
signal connection_toggled
signal deafen_toggled
signal xr_reveal_requested
signal xr_order_changed(above: bool)
signal xr_openvr_changed(enabled: bool)
signal microphone_toggled
signal device_selected(device: String)
signal gain_changed(db: float)
signal name_changed(value: String)
signal smooth_turn_changed(enabled: bool)
signal turn_speed_changed(speed: float)
signal volume_changed(db: float)
signal movie_settings_changed(near_radius: float, far_radius: float)
signal voice_settings_changed(percent: float, near_radius: float, far_radius: float)

const PIXELS := Vector2i(1200, 1200)
const METERS := Vector2(1.5, 1.5)
var desktop_layer: CanvasLayer
var desktop_surface: TextureRect
var quad: MeshInstance3D
var vr_mode := false
var desktop_in_vr := false
var headset_input_allowed := true
var room_connected := false
# Query at selection time: native pickers can stay open across room changes.
var connection_active: Callable
var file_choice: VBoxContainer
var file_choice_label: Label
var selected_file := ""
var return_to_room: Button
var tabs: TabContainer
var share_path: LineEdit
var file_selection_status: Label
var stereo: CheckButton
var subtitles: OptionButton
var avatar_picker: OptionButton
var eye_height: HSlider
var height_label: Label
var avatar_status: Label
var avatar_preview: SubViewport
var avatar_camera: Camera3D
var avatar_mic_button: Button
var avatar_mic_status: Label
var avatar_close_up: CheckButton
var viewport: SubViewport
var url: LineEdit
var current_source: LineEdit
var share_button: Button
var stop_share_button: Button
var relay_rows: VBoxContainer
var relay_signature := ""
var share_summary: Label
var upload_limit: SpinBox
var xr_button: Button
var xr_open_button: Button
var xr_takeover_dialog: ConfirmationDialog
var xr_hide_button: Button
var xr_status: Label
var xr_controls: Label
var deafen_button: Button
var xr_reveal_button: Button
var xr_order: CheckButton
var xr_openvr: CheckButton
var display_name: LineEdit
var connection_button: Button
var mic_button: Button
var play_button: Button
var devices: OptionButton
var meter: ProgressBar
var status: Label
var media_status: Label
var gain: HSlider
var smooth: CheckButton
var speed: HSlider
var volume: HSlider
var movie_near: HSlider
var movie_far: HSlider
var movie_near_label: Label
var movie_far_label: Label
var receive_volume: HSlider
var voice_near: HSlider
var voice_far: HSlider
var receive_volume_label: Label
var voice_near_label: Label
var voice_far_label: Label
var timeline: HSlider
var playback_time: Label
var scrub_timer: Timer
var scrubbing := false
var seek_pending := false
var updating_timeline := false
var last_pressed := false
var last_position := Vector2.ZERO
var has_hit := false

func _ready() -> void:
	viewport = SubViewport.new()
	viewport.size = PIXELS
	viewport.render_target_update_mode = SubViewport.UPDATE_WHEN_VISIBLE
	viewport.gui_disable_input = false
	viewport.gui_embed_subwindows = true
	desktop_layer = CanvasLayer.new()
	desktop_layer.layer = 10
	add_child(desktop_layer)
	# Keep a single standalone viewport in both modes. Both pointers feed the
	# same local-coordinate event path; only the texture's presentation changes.
	add_child(viewport)
	viewport.handle_input_locally = true
	desktop_surface = TextureRect.new()
	desktop_surface.texture = viewport.get_texture()
	desktop_surface.size = Vector2(PIXELS)
	desktop_surface.mouse_filter = Control.MOUSE_FILTER_STOP
	desktop_surface.gui_input.connect(forward_desktop_input)
	desktop_layer.add_child(desktop_surface)
	get_viewport().size_changed.connect(layout_desktop)
	visibility_changed.connect(update_presentation)
	quad = MeshInstance3D.new()
	var mesh := QuadMesh.new()
	mesh.size = METERS
	quad.mesh = mesh
	var material := StandardMaterial3D.new()
	material.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
	material.albedo_texture = viewport.get_texture()
	material.texture_filter = BaseMaterial3D.TEXTURE_FILTER_LINEAR
	material.no_depth_test = true
	material.transparency = BaseMaterial3D.TRANSPARENCY_ALPHA
	material.render_priority = 100
	quad.material_override = material
	add_child(quad)
	var panel := PanelContainer.new()
	panel.set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)
	var theme := Theme.new()
	theme.default_font_size = 25
	panel.theme = theme
	var background := StyleBoxFlat.new()
	background.bg_color = Color("182333")
	background.set_corner_radius_all(16)
	background.content_margin_left = 30
	background.content_margin_right = 30
	background.content_margin_top = 22
	background.content_margin_bottom = 22
	panel.add_theme_stylebox_override("panel", background)
	viewport.add_child(panel)
	tabs = TabContainer.new()
	panel.add_child(tabs)
	build_avatar_menu(tabs)
	var column := VBoxContainer.new()
	column.name = "Room"
	column.add_theme_constant_override("separation", 14)
	tabs.add_child(column)
	tabs.move_child(column, 0)
	tabs.current_tab = 0
	var title := Label.new()
	title.text = "prim  /  friends theater"
	title.add_theme_font_size_override("font_size", 38)
	column.add_child(title)
	status = label(column, "Singleplayer — microphone muted")
	status.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	var row := horizontal(column)
	connection_button = button(row, "Connect to friends", func(): connection_toggled.emit())
	label(row, "Username")
	display_name = LineEdit.new()
	display_name.placeholder_text = "Your name"
	display_name.max_length = 32
	display_name.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	display_name.text_submitted.connect(func(value): name_changed.emit(value))
	display_name.focus_exited.connect(func(): name_changed.emit(display_name.text))
	row.add_child(display_name)
	row = horizontal(column)
	xr_button = button(row, "Enable VR", func(): xr_toggled.emit())
	xr_button.custom_minimum_size = Vector2(260, 58)
	xr_open_button = button(row, "Open Prim in headset", func(): xr_open_requested.emit())
	xr_open_button.visible = false
	xr_takeover_dialog = ConfirmationDialog.new()
	xr_takeover_dialog.title = "Open Prim in headset?"
	xr_takeover_dialog.ok_button_text = "Open Prim"
	xr_takeover_dialog.confirmed.connect(func(): xr_takeover_confirmed.emit())
	add_child(xr_takeover_dialog)
	xr_hide_button = button(row, "Hide room view", func(): xr_hide_requested.emit())
	xr_hide_button.visible = false
	var room_column := column
	column = VBoxContainer.new()
	column.name = "Movie"
	column.add_theme_constant_override("separation", 14)
	tabs.add_child(column)
	tabs.move_child(column, 1)
	label(column, "MOVIE / LIVESTREAM")
	row = horizontal(column)
	label(row, "Current source")
	current_source = LineEdit.new()
	current_source.editable = false
	current_source.placeholder_text = "No media"
	current_source.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	row.add_child(current_source)
	row = horizontal(column)
	url = LineEdit.new()
	url.placeholder_text = "Video URL or local file path"
	url.max_length = 4096
	url.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	url.text_submitted.connect(func(value): choose_source(value))
	row.add_child(url)
	button(row, "Paste", func(): url.text = DisplayServer.clipboard_get().strip_edges())
	button(row, "Open", func(): choose_source(url.text))
	button(row, "Browse…", browse_file)
	var share_column := VBoxContainer.new()
	share_column.name = "Sharing"
	share_column.add_theme_constant_override("separation", 14)
	tabs.add_child(share_column)
	label(share_column, "FILE TRANSFERS")
	label(share_column, "Choose a file on the Movie tab to share it with the room.")
	# One source field is shared by URL, pasted paths, browsing, and file drops.
	share_path = url
	file_selection_status = label(column, "Open a URL, browse, or drop one video file onto the window.")
	file_selection_status.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	file_choice = VBoxContainer.new()
	column.add_child(file_choice)
	file_choice_label = label(file_choice, "Share this file with the room?")
	file_choice_label.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	row = horizontal(file_choice)
	share_button = button(row, "Share with room", func():
		file_choice.hide()
		share_file_requested.emit(selected_file))
	button(row, "Play only here", func():
		file_choice.hide()
		local_file_requested.emit(selected_file))
	button(row, "Cancel", func(): file_choice.hide())
	file_choice.hide()
	return_to_room = button(column, "Playing only here • Return to room playback", func(): room_playback_requested.emit())
	return_to_room.hide()
	row = horizontal(share_column)
	stop_share_button = button(row, "Stop sharing", func(): stop_share_requested.emit())
	label(row, "Upload limit (Mbit/s)")
	upload_limit = SpinBox.new()
	upload_limit.min_value = 1
	upload_limit.max_value = 1000
	upload_limit.value = 100
	upload_limit.value_changed.connect(func(value): media_limit_changed.emit(int(value)))
	row.add_child(upload_limit)
	share_summary = label(share_column, "Connect to friends, choose a file, then Share with room.")
	var relay_help := label(share_column, "Relayed video uses relay-server bandwidth. Each viewer needs your approval for this share; voice stays connected.")
	relay_help.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	relay_rows = VBoxContainer.new()
	share_column.add_child(relay_rows)
	row = horizontal(column)
	play_button = button(row, "Play", func(): playback_toggled.emit())
	timeline = slider(row, 0, 1, 0, 0.1)
	timeline.custom_minimum_size.y = 46
	timeline.editable = false
	timeline.drag_started.connect(func(): scrubbing = true)
	timeline.drag_ended.connect(func(_changed):
		scrubbing = false
		flush_seek())
	timeline.value_changed.connect(func(_value):
		if updating_timeline: return
		seek_pending = true
		scrub_timer.start()
		update_time_label())
	scrub_timer = Timer.new()
	scrub_timer.one_shot = true
	scrub_timer.wait_time = 0.2
	scrub_timer.timeout.connect(flush_seek)
	add_child(scrub_timer)
	playback_time = label(row, "0:00 / 0:00")
	row = horizontal(column)
	stereo = CheckButton.new()
	stereo.text = "Direct stereo"
	stereo.toggled.connect(func(value): stereo_changed.emit(value))
	row.add_child(stereo)
	subtitles = OptionButton.new()
	subtitles.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	subtitles.fit_to_longest_item = false
	subtitles.clip_text = true
	subtitles.add_item("Subtitles: none available", -1)
	subtitles.disabled = true
	subtitles.item_selected.connect(func(index): subtitle_selected.emit(subtitles.get_item_id(index)))
	row.add_child(subtitles)
	row = horizontal(column)
	label(row, "Video volume")
	volume = slider(row, -40, 6, 0, 1)
	volume.value_changed.connect(func(value): volume_changed.emit(value))
	row = horizontal(column)
	label(row, "Full volume within")
	movie_near = slider(row, 0.5, 12, 6, 0.5)
	movie_near_label = label(row, "6.0 m")
	movie_near_label.custom_minimum_size.x = 85
	label(row, "Silent beyond")
	movie_far = slider(row, 1, 40, 18, 0.5)
	movie_far_label = label(row, "18.0 m")
	movie_far_label.custom_minimum_size.x = 85
	movie_near.value_changed.connect(func(value):
		movie_far.set_value_no_signal(maxf(movie_far.value, value + 0.5))
		emit_movie_settings())
	movie_far.value_changed.connect(func(value):
		movie_near.set_value_no_signal(minf(movie_near.value, value - 0.5))
		emit_movie_settings())
	media_status = label(column, "Choose a video to begin.")
	media_status.text_overrun_behavior = TextServer.OVERRUN_TRIM_ELLIPSIS
	media_status.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	media_status.max_lines_visible = 2
	column = room_column
	column.add_child(HSeparator.new())
	label(column, "VOICE")
	row = horizontal(column)
	mic_button = button(row, "MIC MUTED • Unmute", func(): microphone_toggled.emit())
	deafen_button = button(row, "LISTENING • Deafen", func(): deafen_toggled.emit())
	deafen_button.tooltip_text = "Silence Prim voices and media. Your microphone is unchanged."
	mic_button.custom_minimum_size = Vector2(450, 80)
	mic_button.add_theme_font_size_override("font_size", 32)
	row = horizontal(column)
	devices = OptionButton.new()
	devices.fit_to_longest_item = false
	devices.clip_text = true
	devices.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	devices.item_selected.connect(func(index): device_selected.emit(devices.get_item_text(index)))
	row.add_child(devices)
	button(row, "Refresh", refresh_devices)
	refresh_devices()
	row = horizontal(column)
	label(row, "Mic gain (dB)")
	gain = slider(row, -30, 24, 0, 1)
	gain.value_changed.connect(func(value): gain_changed.emit(value))
	meter = ProgressBar.new()
	meter.min_value = -60
	meter.max_value = 0
	meter.value = -60
	meter.show_percentage = false
	meter.custom_minimum_size = Vector2(280, 34)
	row.add_child(meter)
	row = horizontal(column)
	label(row, "Received voices")
	receive_volume = slider(row, 0, 300, 150, 5)
	receive_volume_label = label(row, "150%")
	receive_volume_label.custom_minimum_size.x = 80
	row = horizontal(column)
	label(row, "Full volume within")
	voice_near = slider(row, 0.5, 12, 3, 0.5)
	voice_near_label = label(row, "3.0 m")
	voice_near_label.custom_minimum_size.x = 85
	label(row, "Silent beyond")
	voice_far = slider(row, 1, 40, 15, 0.5)
	voice_far_label = label(row, "15.0 m")
	voice_far_label.custom_minimum_size.x = 85
	receive_volume.value_changed.connect(func(_value): emit_voice_settings())
	voice_near.value_changed.connect(func(value):
		voice_far.set_value_no_signal(maxf(voice_far.value, value + 0.5))
		emit_voice_settings())
	voice_far.value_changed.connect(func(value):
		voice_near.set_value_no_signal(minf(voice_near.value, value - 0.5))
		emit_voice_settings())
	label(column, "Use headphones. Voice starts muted whenever you join.")
	column.add_child(HSeparator.new())
	column = VBoxContainer.new()
	column.name = "Comfort"
	xr_status = label(column, "Desktop")
	xr_status.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	column.add_theme_constant_override("separation", 14)
	tabs.add_child(column)
	label(column, "COMFORT")
	xr_order = CheckButton.new()
	xr_order.text = "Draw Prim above other overlays"
	xr_order.tooltip_text = "Compatibility with unpatched WayVR. Covers its panels while Prim is visible. Changing this restarts VR only."
	xr_order.toggled.connect(func(value): xr_order_changed.emit(value))
	column.add_child(xr_order)
	xr_openvr = CheckButton.new()
	xr_openvr.text = "Experimental SteamVR peek"
	xr_openvr.tooltip_text = "Stereo projective overlay while a game runs. Disable to use tracking only. Restarts VR."
	xr_openvr.disabled = not ClassDB.class_exists("XRInterfaceOpenVROverlay")
	xr_openvr.toggled.connect(func(value): xr_openvr_changed.emit(value))
	column.add_child(xr_openvr)
	xr_reveal_button = button(column, "Reveal Prim room", func(): xr_reveal_requested.emit())
	xr_reveal_button.visible = false
	row = horizontal(column)
	smooth = CheckButton.new()
	smooth.text = "Smooth turn"
	smooth.toggled.connect(func(value): smooth_turn_changed.emit(value))
	row.add_child(smooth)
	label(row, "Turn speed (°/s)")
	speed = slider(row, 15, 150, 60, 5)
	speed.value_changed.connect(func(value): turn_speed_changed.emit(value))
	xr_controls = label(column, "VR: Y/B menu • A/X mic • trigger selects • sticks move / turn")
	label(column, "Desktop: WASD + mouse • Tab or Esc menu • click selects")
	layout_desktop()
	set_microphone_muted(true)
	update_presentation()

func layout_desktop() -> void:
	var available := get_viewport().get_visible_rect().size
	var factor := minf((available.x - 32) / PIXELS.x, (available.y - 32) / PIXELS.y)
	factor = clampf(factor, 0.1, 1.0)
	desktop_surface.scale = Vector2.ONE * factor
	desktop_surface.position = (available - Vector2(PIXELS) * factor) * 0.5

func set_vr_mode(enabled: bool) -> void:
	release_pointer()
	vr_mode = enabled
	desktop_in_vr = false
	headset_input_allowed = true
	var focus := viewport.gui_get_focus_owner()
	if focus: focus.release_focus()
	update_presentation()

func update_presentation() -> void:
	if not is_instance_valid(quad): return
	quad.visible = vr_mode and visible and not desktop_in_vr and headset_input_allowed
	desktop_surface.visible = (not vr_mode or desktop_in_vr) and visible
	viewport.render_target_update_mode = SubViewport.UPDATE_ALWAYS if visible else SubViewport.UPDATE_DISABLED
	layout_desktop()
	if vr_mode:
		Input.mouse_mode = Input.MOUSE_MODE_VISIBLE
	else:
		Input.mouse_mode = Input.MOUSE_MODE_VISIBLE if visible or not get_window().has_focus() else Input.MOUSE_MODE_CAPTURED

func set_microphone_muted(muted: bool) -> void:
	mic_button.text = "MIC MUTED • Unmute" if muted else "MIC ON • Mute"
	var style := StyleBoxFlat.new()
	style.bg_color = Color("74434a") if muted else Color("246950")
	style.set_corner_radius_all(8)
	mic_button.add_theme_stylebox_override("normal", style)

func horizontal(parent: Node) -> HBoxContainer:
	var row := HBoxContainer.new()
	row.add_theme_constant_override("separation", 12)
	parent.add_child(row)
	return row

func label(parent: Node, text: String) -> Label:
	var result := Label.new()
	result.text = text
	parent.add_child(result)
	return result

func button(parent: Node, text: String, action: Callable) -> Button:
	var result := Button.new()
	result.text = text
	result.custom_minimum_size.y = 46
	result.pressed.connect(action)
	parent.add_child(result)
	return result

func slider(parent: Node, low: float, high: float, initial: float, step: float) -> HSlider:
	var result := HSlider.new()
	result.min_value = low
	result.max_value = high
	result.step = step
	result.value = initial
	result.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	result.custom_minimum_size.x = 180
	parent.add_child(result)
	return result

func refresh_devices() -> void:
	var selected := AudioServer.input_device
	devices.clear()
	for device in AudioServer.get_input_device_list():
		devices.add_item(device)
		if device == selected:
			devices.select(devices.item_count - 1)

func text_focused() -> bool:
	return viewport != null and viewport.gui_get_focus_owner() is LineEdit

func forward_desktop_input(event: InputEvent) -> void:
	if not desktop_surface.visible: return
	if event is InputEventMouse:
		# Control.gui_input has already removed the desktop panel's scale/offset.
		viewport.push_input(event, true)
		desktop_surface.accept_event()

func forward_key(event: InputEvent) -> void:
	if visible:
		viewport.push_input(event, true)

func open_at(camera: Camera3D) -> void:
	var forward := -camera.global_basis.z
	forward.y = 0
	forward = forward.normalized()
	global_position = camera.global_position + forward * 1.65
	look_at(camera.global_position, Vector3.UP, true)
	visible = true

func point(origin: Vector3, direction: Vector3, pressed: bool) -> Vector3:
	var local_origin := global_transform.affine_inverse() * origin
	var local_direction := global_basis.inverse() * direction
	has_hit = false
	var target := origin + direction * 5
	if quad.visible and absf(local_direction.z) > 0.0001:
		var distance := -local_origin.z / local_direction.z
		var hit := local_origin + local_direction * distance
		if distance > 0 and absf(hit.x) < METERS.x * 0.5 and absf(hit.y) < METERS.y * 0.5:
			has_hit = true
			target = global_transform * hit
			var pixel := Vector2((hit.x / METERS.x + 0.5) * PIXELS.x, (0.5 - hit.y / METERS.y) * PIXELS.y)
			var motion := InputEventMouseMotion.new()
			motion.position = pixel
			motion.global_position = pixel
			motion.relative = pixel - last_position
			motion.button_mask = MOUSE_BUTTON_MASK_LEFT if last_pressed else 0
			viewport.push_input(motion, true)
			last_position = pixel
	var active := pressed and has_hit
	if active != last_pressed:
		var click := InputEventMouseButton.new()
		click.button_index = MOUSE_BUTTON_LEFT
		click.pressed = active
		click.position = last_position
		click.global_position = last_position
		viewport.push_input(click, true)
		last_pressed = active
	return target

func flush_seek() -> void:
	if not seek_pending or not timeline.editable: return
	seek_pending = false
	scrub_timer.stop()
	seek_requested.emit(timeline.value)

func update_playback(position: float, duration: float, available: bool) -> void:
	var valid := available and is_finite(duration) and duration > 0
	timeline.editable = valid
	if not valid:
		scrubbing = false
		seek_pending = false
		scrub_timer.stop()
	if not scrubbing and not seek_pending:
		updating_timeline = true
		timeline.max_value = maxf(1.0, duration) if valid else 1.0
		timeline.set_value_no_signal(clampf(position, 0, duration) if valid else 0.0)
		updating_timeline = false
	update_time_label()

func update_time_label() -> void:
	playback_time.text = "%s / %s" % [format_time(timeline.value), format_time(timeline.max_value if timeline.editable else 0)]

static func format_time(seconds: float) -> String:
	var total := maxi(0, int(seconds))
	if total >= 3600: return "%d:%02d:%02d" % [total / 3600, (total / 60) % 60, total % 60]
	return "%d:%02d" % [total / 60, total % 60]

func set_voice_settings(percent: float, near_radius: float, far_radius: float) -> void:
	receive_volume.set_value_no_signal(percent)
	voice_near.set_value_no_signal(near_radius)
	voice_far.set_value_no_signal(far_radius)
	refresh_voice_labels()

func refresh_voice_labels() -> void:
	receive_volume_label.text = "%d%%" % int(receive_volume.value)
	voice_near_label.text = "%.1f m" % voice_near.value
	voice_far_label.text = "%.1f m" % voice_far.value

func emit_voice_settings() -> void:
	refresh_voice_labels()
	voice_settings_changed.emit(receive_volume.value, voice_near.value, voice_far.value)

func set_movie_settings(near_radius: float, far_radius: float) -> void:
	movie_near.set_value_no_signal(near_radius)
	movie_far.set_value_no_signal(far_radius)
	refresh_movie_labels()

func refresh_movie_labels() -> void:
	movie_near_label.text = "%.1f m" % movie_near.value
	movie_far_label.text = "%.1f m" % movie_far.value

func emit_movie_settings() -> void:
	refresh_movie_labels()
	movie_settings_changed.emit(movie_near.value, movie_far.value)

func build_avatar_menu(tabs: TabContainer) -> void:
	var column := VBoxContainer.new()
	column.name = "Avatar"
	column.add_theme_constant_override("separation", 18)
	tabs.add_child(column)
	label(column, "AVATAR")
	avatar_picker = OptionButton.new()
	var catalog = preload("res://avatars/catalog.gd")
	for id in catalog.MODELS:
		avatar_picker.add_item(catalog.MODELS[id].name)
		avatar_picker.set_item_metadata(avatar_picker.item_count - 1, id)
	avatar_picker.item_selected.connect(func(index): avatar_selected.emit(avatar_picker.get_item_metadata(index)))
	column.add_child(avatar_picker)
	var row := horizontal(column)
	label(row, "Standing eye height")
	eye_height = slider(row, 0.5, 2.5, 1.6, 0.01)
	height_label = label(row, "1.60 m")
	var height_commit := Timer.new()
	height_commit.one_shot = true
	height_commit.wait_time = 0.3
	column.add_child(height_commit)
	height_commit.timeout.connect(func(): height_changed.emit(eye_height.value))
	eye_height.value_changed.connect(func(value):
		height_label.text = "%.2f m" % value
		height_commit.start())
	button(column, "Measure / recalibrate standing height", func(): calibration_requested.emit())
	avatar_status = label(column, "Stand upright and look forward when measuring. Headset and hands only.")
	avatar_status.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	row = horizontal(column)
	avatar_mic_button = button(row, "Unmute microphone", func(): microphone_toggled.emit())
	avatar_close_up = CheckButton.new()
	avatar_close_up.text = "Face close-up"
	row.add_child(avatar_close_up)
	avatar_mic_status = label(column, "Speak to animate your avatar. Local preview only.")
	avatar_mic_status.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	avatar_preview = SubViewport.new()
	avatar_preview.size = Vector2i(700,700)
	avatar_preview.transparent_bg = false
	avatar_preview.msaa_3d = Viewport.MSAA_4X
	avatar_preview.render_target_update_mode = SubViewport.UPDATE_WHEN_VISIBLE
	column.add_child(avatar_preview)
	avatar_camera = Camera3D.new()
	avatar_camera.fov = 40
	avatar_camera.cull_mask = preload("res://avatars/driver.gd").LOCAL_THIRD
	avatar_preview.add_child(avatar_camera)
	avatar_camera.make_current()
	var preview := TextureRect.new()
	preview.texture = avatar_preview.get_texture()
	preview.flip_h = true
	preview.expand_mode = TextureRect.EXPAND_IGNORE_SIZE
	preview.stretch_mode = TextureRect.STRETCH_KEEP_ASPECT_CENTERED
	preview.custom_minimum_size = Vector2(650,650)
	preview.size_flags_vertical = Control.SIZE_EXPAND_FILL
	column.add_child(preview)
	label(column, "Experimental standing avatars • Alicia fit based on Mainspring")

func set_avatar_settings(id: String, height: float) -> void:
	for i in range(avatar_picker.item_count):
		if avatar_picker.get_item_metadata(i) == id: avatar_picker.select(i)
	eye_height.set_value_no_signal(height)
	height_label.text = "%.2f m" % height

func update_media_share(connected: bool, state: Dictionary) -> void:
	room_connected = connected
	share_button.disabled = not connected
	stop_share_button.disabled = not connected or not state.get("hosted", false)
	upload_limit.editable = connected
	var viewers: Dictionary = state.get("viewers", {})
	var signature := ""
	var rows: Array[Dictionary] = []
	var direct := 0
	var relayed := 0
	for peer in viewers:
		var viewer: Dictionary = viewers[peer]
		if viewer.get("path") == "direct": direct += 1
		if viewer.get("path") == "relay":
			relayed += 1
			rows.append(viewer.duplicate())
		if viewer.get("consent") == "allowed": signature += peer + "allowed"
	rows.sort_custom(func(a, b): return a.peer < b.peer)
	var descriptor: Variant = state.get("publication")
	var media_id: String = descriptor.id if descriptor is Dictionary else ""
	for row in rows: row.erase("bytes_sent")
	signature += media_id + JSON.stringify(rows)
	if signature != relay_signature:
		relay_signature = signature
		for child in relay_rows.get_children(): relay_rows.remove_child(child); child.queue_free()
		for viewer in rows:
			var row := horizontal(relay_rows)
			var notice := label(row, "%s • relay %s for this share" % [viewer.get("name", "Friend"), viewer.get("consent", "pending")])
			notice.size_flags_horizontal = Control.SIZE_EXPAND_FILL
			notice.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
			var peer: String = viewer.peer
			button(row, "Allow", func(): relay_decided.emit(media_id, peer, true))
			button(row, "Direct only", func(): relay_decided.emit(media_id, peer, false))
	if state.get("hosted", false):
		share_summary.text = "Sharing • %d direct • %d relay paths • relay estimate %.1f Mbit/s total\nFile average only; peaks can be higher. Upload capped at %d Mbit/s." % [direct, relayed, relayed * float(state.get("average_mbps", 0)), int(upload_limit.value)]
	elif connected: share_summary.text = "Choose a file on the Movie tab to share with friends."
	else: share_summary.text = state.get("status", "Connect to friends to share files.")

static func normalize_file_path(value: String) -> String:
	value = value.strip_edges()
	if value.length() >= 2 and ((value.begins_with('"') and value.ends_with('"')) or (value.begins_with("'") and value.ends_with("'"))):
		value = value.substr(1, value.length() - 2)
	if value.begins_with("file://"):
		value = value.substr(7)
		if value.begins_with("localhost/"): value = value.substr(9)
		if not value.begins_with("/"): return ""
		for i in range(value.length()):
			if value[i] == "%" and (i + 2 >= value.length() or not value.substr(i + 1, 2).is_valid_hex_number()): return ""
		value = value.uri_decode()
		if OS.get_name() == "Windows" and value.length() >= 3 and value[2] == ":": value = value.substr(1)
	if value.contains("\n") or value.contains("\r"): return ""
	return value

func choose_source(value: String) -> void:
	file_choice.hide()
	if PrimPlayback.is_remote_source(value.strip_edges()):
		source_requested.emit(value.strip_edges())
	else:
		select_file(value)

func select_file(path: String) -> void:
	url.text = normalize_file_path(path)
	select_tab("Movie")
	if not FileAccess.file_exists(url.text):
		file_selection_status.text = "Choose an existing local file."
		file_choice.hide()
		return
	selected_file = url.text
	file_selection_status.text = "Selected: " + selected_file.get_file()
	var connected: bool = connection_active.call() if connection_active.is_valid() else room_connected
	share_button.disabled = not connected
	if connected:
		file_choice_label.text = "Share %s with the room, or play it only here?" % url.text.get_file()
		file_choice.show()
	else:
		local_file_requested.emit(url.text)

func select_tab(title: String) -> void:
	for i in range(tabs.get_tab_count()):
		if tabs.get_tab_title(i) == title: tabs.current_tab = i

func browse_file() -> void:
	var picker := FileDialog.new()
	picker.access = FileDialog.ACCESS_FILESYSTEM
	picker.file_mode = FileDialog.FILE_MODE_OPEN_FILE
	picker.use_native_dialog = true
	picker.title = "Open a video"
	picker.file_selected.connect(func(path): select_file(path); picker.queue_free())
	picker.canceled.connect(picker.queue_free)
	add_child(picker)
	file_selection_status.text = "File picker opened on the desktop."
	picker.popup_centered_ratio(0.7)

func accept_file_drop(files: PackedStringArray, camera: Camera3D) -> void:
	open_at(camera)
	select_tab("Movie")
	if files.size() != 1:
		file_selection_status.text = "Drop one video file at a time."
		return
	select_file(files[0])

func update_subtitles(tracks: Array, selected: int) -> void:
	subtitles.clear()
	subtitles.add_item("Subtitles: Off", 0)
	subtitles.set_item_id(0, -1)
	subtitles.disabled = tracks.is_empty()
	if tracks.is_empty(): subtitles.set_item_text(0, "Subtitles: none available")
	for track in tracks:
		var text := " · ".join([str(track.get("lang", "und")), str(track.get("title", "Track %s" % track.id))])
		subtitles.add_item(text, int(track.id))
		if int(track.id) == selected: subtitles.select(subtitles.item_count - 1)

func set_xr_state(state: String, message: String) -> void:
	xr_button.text = {"desktop": "Enable VR", "starting": "Enabling VR…", "xr": "Disable VR", "background": "Disable VR", "stopping": "Disabling VR…"}.get(state, "Enable VR")
	xr_button.disabled = state == "starting" or state == "stopping"
	xr_open_button.visible = state == "background"
	if state != "background": xr_takeover_dialog.hide()
	xr_status.text = message
	xr_button.tooltip_text = message
	if state != "xr":
		xr_hide_button.visible = false
		xr_reveal_button.visible = false

func confirm_xr_takeover(app: String) -> void:
	if app.is_empty(): app = "the current VR app"
	xr_takeover_dialog.dialog_text = "SteamVR may close %s to open Prim. Continue?" % app
	xr_takeover_dialog.popup_centered(Vector2i(480, 160))

func release_pointer() -> void:
	if last_pressed and is_instance_valid(viewport):
		var event := InputEventMouseButton.new()
		event.button_index = MOUSE_BUTTON_LEFT
		event.position = last_position
		event.global_position = last_position
		event.pressed = false
		viewport.push_input(event, true)
	last_pressed = false

func set_headset_input_allowed(allowed: bool) -> void:
	if headset_input_allowed == allowed: return
	release_pointer()
	headset_input_allowed = allowed
	if not allowed and vr_mode and not desktop_in_vr: visible = false
	update_presentation()

func open_desktop() -> void:
	release_pointer()
	desktop_in_vr = vr_mode
	visible = true
	update_presentation()

func open_headset(camera: Camera3D) -> void:
	if not headset_input_allowed: return
	release_pointer()
	desktop_in_vr = false
	open_at(camera)
	update_presentation()

func set_deafened(value: bool) -> void:
	deafen_button.text = "DEAFENED • Listen" if value else "LISTENING • Deafen"
	deafen_button.modulate = Color("ffbf75") if value else Color.WHITE
