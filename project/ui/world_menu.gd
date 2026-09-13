class_name PrimMenu
extends Node3D

signal avatar_selected(id: String)
signal height_changed(height: float)
signal calibration_requested

signal source_requested(source: String)
signal share_file_requested(path: String)
signal stop_share_requested
signal relay_decided(media_id: String, peer: String, allow: bool)
signal media_limit_changed(mbps: int)
signal playback_toggled
signal seek_requested(seconds: float)
signal connection_toggled
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
var xr_controls: Label
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
	add_child(viewport)
	var quad := MeshInstance3D.new()
	var mesh := QuadMesh.new()
	mesh.size = METERS
	quad.mesh = mesh
	var material := StandardMaterial3D.new()
	material.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
	material.albedo_texture = viewport.get_texture()
	material.texture_filter = BaseMaterial3D.TEXTURE_FILTER_LINEAR
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
	var tabs := TabContainer.new()
	panel.add_child(tabs)
	build_avatar_menu(tabs)
	var column := VBoxContainer.new()
	column.name = "Theater"
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
	column.add_child(HSeparator.new())
	label(column, "VIDEO")
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
	url.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	url.text_submitted.connect(func(value): source_requested.emit(value))
	row.add_child(url)
	button(row, "Paste", func(): url.text = DisplayServer.clipboard_get().strip_edges())
	button(row, "Open", func(): source_requested.emit(url.text))
	var share_column := VBoxContainer.new()
	share_column.name = "Sharing"
	share_column.add_theme_constant_override("separation", 14)
	tabs.add_child(share_column)
	label(share_column, "SHARE A VIDEO FILE")
	var share_path := LineEdit.new()
	share_path.placeholder_text = "Local file path (optional; leave empty to browse)"
	share_path.max_length = 4096
	share_path.text_submitted.connect(func(path): share_file_requested.emit(path))
	share_column.add_child(share_path)
	row = horizontal(share_column)
	button(row, "Paste path", func(): share_path.text = DisplayServer.clipboard_get().strip_edges())
	share_button = button(row, "Share file", func():
		if not share_path.text.strip_edges().is_empty():
			share_file_requested.emit(share_path.text.strip_edges())
		else:
			var picker := FileDialog.new()
			picker.access = FileDialog.ACCESS_FILESYSTEM
			picker.file_mode = FileDialog.FILE_MODE_OPEN_FILE
			picker.use_native_dialog = true
			picker.title = "Share a video with the room"
			picker.file_selected.connect(func(path): share_file_requested.emit(path); picker.queue_free())
			picker.canceled.connect(picker.queue_free)
			add_child(picker)
			picker.popup_centered_ratio(0.7))
	stop_share_button = button(row, "Stop sharing", func(): stop_share_requested.emit())
	label(row, "Upload limit (Mbit/s)")
	upload_limit = SpinBox.new()
	upload_limit.min_value = 1
	upload_limit.max_value = 1000
	upload_limit.value = 100
	upload_limit.value_changed.connect(func(value): media_limit_changed.emit(int(value)))
	row.add_child(upload_limit)
	share_summary = label(share_column, "Connect as host to share a file. In VR you can paste a local path above.")
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
	column.add_child(HSeparator.new())
	label(column, "VOICE")
	row = horizontal(column)
	mic_button = button(row, "Unmute microphone", func(): microphone_toggled.emit())
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
	label(column, "COMFORT")
	row = horizontal(column)
	smooth = CheckButton.new()
	smooth.text = "Smooth turn"
	smooth.toggled.connect(func(value): smooth_turn_changed.emit(value))
	row.add_child(smooth)
	label(row, "Turn speed (°/s)")
	speed = slider(row, 15, 150, 60, 5)
	speed.value_changed.connect(func(value): turn_speed_changed.emit(value))
	xr_controls = label(column, "VR: Y/B menu • A/X mic • trigger selects • sticks move / turn")
	label(column, "Desktop: WASD + mouse • Tab menu • click selects • Esc cursor")

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

func forward_key(event: InputEvent) -> void:
	if visible and text_focused():
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
	if visible and absf(local_direction.z) > 0.0001:
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

func update_media_share(host: bool, state: Dictionary) -> void:
	share_button.disabled = not host
	stop_share_button.disabled = not host or not state.get("hosted", false)
	upload_limit.editable = host
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
	var descriptor: Variant = state.get("descriptor")
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
	elif host: share_summary.text = "Share a video file, or paste its path above."
	else: share_summary.text = state.get("status", "Only the room host can share files.")
