## Owns XR providers. The room, media and microphone outlive their sessions.
extends Node

signal changed(state: String, detail: String)
signal mode_changed(in_vr: bool)
signal background_mode_changed(active: bool)
signal takeover_confirmation_requested(app: String)
signal presentation_changed(primary_room: bool, amount: float)

const OpenVRBridge = preload("res://xr/openvr_bridge.gd")
var enabled_intent := false
var bridge: RefCounted
var background_idle_since := -1
var auto_scene_blocked := false
var state := "desktop"
var detail := "Desktop • enable VR whenever your headset is ready."
var interface: XRInterface
var openxr_interface: XRInterface
var openvr_interface: XRInterface
var openvr_peek := false
var openvr_active := false
var openvr_failed := false
var quitting := false
var desktop_vsync: int
var desktop_max_fps: int
var render_viewport: Viewport
var monitor: RefCounted
var observation := {"activity": "unknown", "runtime": "Undetected"}
var overlay_above := false
var manual_reveal := false
var overlay_active := false
var room_primary := false
var head_valid := false
var reveal_fraction := 0.0
var presentation_fraction := 0.0
var idle_elapsed := 0.0
var visibility_serial := -1
var event_elapsed := 0.0
var observe_elapsed := 0.0
var runtime_name := "OpenXR"
var gesture_epoch := 0
var activity_key := ""


func _ready() -> void:
	desktop_vsync = DisplayServer.window_get_vsync_mode()
	desktop_max_fps = Engine.max_fps
	if render_viewport == null: render_viewport = get_viewport()
	if ClassDB.class_exists("PrimXRMonitor"): monitor = ClassDB.instantiate("PrimXRMonitor")
	interface = XRServer.find_interface("OpenXR")
	openxr_interface = interface
	if ClassDB.class_exists("XRInterfaceOpenVROverlay"):
		openvr_interface = ClassDB.instantiate("XRInterfaceOpenVROverlay")
		openvr_interface.set_overlay_key("space.hiina.prim." + str(OS.get_process_id()))
		XRServer.add_interface(openvr_interface)
	if interface:
		interface.session_stopping.connect(func(): _runtime_ended.call_deferred(false))
		interface.session_loss_pending.connect(func(): _runtime_ended.call_deferred(true))
		interface.instance_exiting.connect(func(): _runtime_ended.call_deferred(false))
	get_tree().auto_accept_quit = false
	get_window().close_requested.connect(close)

func _set_background_mode(active: bool) -> void:
	# A compositor can throttle an unfocused VSync window. Physical tracking and
	# avatar transport must keep ticking while the scene game owns the headset.
	DisplayServer.window_set_vsync_mode(DisplayServer.VSYNC_DISABLED if active else desktop_vsync)
	Engine.max_fps = mini(desktop_max_fps, 60) if active and desktop_max_fps > 0 else (60 if active else desktop_max_fps)
	background_mode_changed.emit(active)

func _set_state(value: String, message: String) -> void:
	state = value
	detail = message
	changed.emit(state, detail)
	print("PRIM_XR state=", state, " detail=", detail)

func toggle() -> void:
	if enabled_intent or state in ["xr", "background", "starting"]: stop()
	elif state == "desktop": start()

func start() -> void:
	if state != "desktop" or quitting: return
	enabled_intent = true
	auto_scene_blocked = false
	openvr_failed = false
	var configured: Dictionary = observation
	if monitor and monitor.has_method("configured_runtime_json"):
		var value = JSON.parse_string(monitor.call("configured_runtime_json"))
		if value is Dictionary: configured = value
	if configured.get("runtime", "") == "SteamVR" and OS.get_environment("PRIM_XR_SCENE") != "1":
		runtime_name = "SteamVR"
		bridge = OpenVRBridge.new()
		if not bridge.start():
			enabled_intent = false
			_set_state("desktop", bridge.error)
			bridge = null
			return
		_set_background_mode(true)
		_set_state("background", "SteamVR • Checking active app…")
		return
	_start_scene()

func _start_scene(force := false) -> void:
	if state not in ["desktop", "background"] or not enabled_intent or quitting: return
	if not interface or not interface.has_method("request_session_exit"):
		enabled_intent = false
		if bridge: bridge.stop(); bridge = null
		_set_background_mode(false)
		_set_state("desktop", "VR unavailable in this build. Use Prim's patched engine.")
		return
	_set_state("starting", "Starting VR…")
	await get_tree().process_frame
	if state != "starting" or not enabled_intent: return
	# Recheck a recent observation immediately before claiming the scene. A
	# starting game or stale/unknown state is never an automatic takeover.
	if bridge:
		var old_sequence := int(bridge.snapshot().get("seq", -1))
		var until := Time.get_ticks_msec() + 1000
		while bridge and state == "starting" and Time.get_ticks_msec() < until:
			bridge.poll()
			if int(bridge.snapshot().get("seq", -1)) > old_sequence: break
			await get_tree().process_frame
		if not bridge or state != "starting" or not enabled_intent: return
		bridge.poll()
		observation = bridge.snapshot()
		if not force and observation.get("activity", "unknown") != "idle":
			_set_state("background", "SteamVR • Waiting for active app")
			return
	if interface.has_method("configure_overlay"):
		interface.call("configure_overlay", OS.get_name() == "Linux" and OS.get_environment("PRIM_XR_SCENE") != "1", 10 if overlay_above else 1)
	if not interface.initialize():
		interface.uninitialize()
		if bridge:
			auto_scene_blocked = true
			_set_state("background", "SteamVR • Could not open Prim room. Tracking continues; retry with Open Prim in headset.")
		else:
			enabled_intent = false
			_set_state("desktop", "Could not start VR. Check your runtime and headset, then try again.")
		return
	overlay_active = interface.has_method("get_overlay_status") and bool(interface.call("get_overlay_status").get("active", false))
	runtime_name = str(interface.get_system_info().get("XRRuntimeName", "OpenXR"))
	if "monado" in runtime_name.to_lower(): runtime_name = "Monado"
	elif "steam" in runtime_name.to_lower(): runtime_name = "SteamVR"
	if monitor: monitor.call("set_monado_session_active", "monado" in runtime_name.to_lower())
	if bridge: _set_background_mode(false)
	head_valid = false
	room_primary = not overlay_active
	idle_elapsed = 0.0
	event_elapsed = 0.0
	visibility_serial = -1
	reveal_fraction = 0.0
	render_viewport.use_xr = true
	DisplayServer.window_set_vsync_mode(DisplayServer.VSYNC_DISABLED)
	mode_changed.emit(true)
	_set_state("xr", "%s • %s" % [runtime_name, "Checking activity…" if overlay_active else "Prim room • companion overlay unavailable"])
	presentation_changed.emit(room_primary, 0.0)

func _runtime_ended(lost: bool) -> void:
	if state == "xr": stop(lost, bridge != null and not lost and enabled_intent)

func stop(runtime_lost := false, preserve_intent := false) -> void:
	if not preserve_intent: enabled_intent = false
	if state == "stopping": return
	if state in ["desktop", "background"] or (state == "starting" and (not interface or not interface.is_initialized())):
		if bridge: bridge.stop(); bridge = null
		_set_background_mode(false)
		_set_state("desktop", "Desktop • enable VR whenever your headset is ready.")
		return
	manual_reveal = false
	_set_state("stopping", "Returning to desktop…")
	if interface and interface.is_initialized() and not runtime_lost and not openvr_active:
		var requested: bool = interface.call("request_session_exit")
		var deadline := Time.get_ticks_msec() + 3000
		while requested and Time.get_ticks_msec() < deadline:
			var session_state: int = interface.get_session_state()
			if session_state == OpenXRInterface.SESSION_STATE_EXITING or session_state == OpenXRInterface.SESSION_STATE_LOSS_PENDING: break
			await get_tree().process_frame
	# Switch the listener/camera before draining frames; room audio keeps running.
	render_viewport.use_xr = false
	overlay_active = false
	room_primary = false
	reveal_fraction = 0.0
	presentation_fraction = 0.0
	presentation_changed.emit(false, 0.0)
	if monitor: monitor.call("set_monado_session_active", false)
	# Godot's XR path replaces the Window viewport size with the headset target
	# and clears its 2D size override. Disabling use_xr alone leaves those behind.
	# Reapply the existing content settings to restore desktop rendering and mouse
	# coordinates, even when the native window has not received a resize event.
	var window := get_window()
	window.content_scale_size = window.content_scale_size
	mode_changed.emit(false)
	DisplayServer.window_set_vsync_mode(DisplayServer.VSYNC_DISABLED if preserve_intent else desktop_vsync)
	var device := RenderingServer.get_rendering_device()
	if device:
		for _frame in range(device.get_frame_delay() + 1):
			await RenderingServer.frame_post_draw
	RenderingServer.force_sync()
	if interface: interface.uninitialize()
	if openvr_active:
		openvr_active = false
		interface = openxr_interface
	if preserve_intent and enabled_intent and bridge:
		background_idle_since = -1
		_set_background_mode(true)
		_set_state("background", "SteamVR • Tracking alongside game")
	else:
		if bridge: bridge.stop(); bridge = null
		_set_background_mode(false)
		_set_state("desktop", "VR runtime disconnected. You are still in Prim." if runtime_lost else "Desktop • enable VR whenever your headset is ready.")

func set_openvr_peek(value: bool) -> void:
	if value == openvr_peek: return
	openvr_peek = value
	if enabled_intent and runtime_name == "SteamVR":
		await stop()
		if not quitting: start()

func _start_openvr_overlay() -> void:
	if state != "background" or not openvr_interface: return
	_set_state("starting", "Starting experimental SteamVR peek…")
	if not openvr_interface.initialize():
		openvr_failed = true
		_set_state("background", "SteamVR peek unavailable • tracking continues")
		return
	interface = openvr_interface
	openvr_active = true
	overlay_active = true
	room_primary = false
	manual_reveal = false
	head_valid = false
	reveal_fraction = 0.0
	presentation_fraction = 0.0
	background_idle_since = -1
	activity_key = ""
	gesture_epoch += 1
	_set_background_mode(false)
	render_viewport.use_xr = true
	DisplayServer.window_set_vsync_mode(DisplayServer.VSYNC_DISABLED)
	Engine.max_fps = 90
	mode_changed.emit(true)
	_set_state("xr", "SteamVR • experimental peek • lift to reveal")
	presentation_changed.emit(false, 0.0)
	if render_viewport is SubViewport: render_viewport.render_target_update_mode = SubViewport.UPDATE_DISABLED

func close() -> void:
	if quitting: return
	quitting = true
	if state != "desktop":
		stop()
		while state != "desktop": await get_tree().process_frame
	get_tree().quit()

func set_head_valid(valid: bool) -> void:
	head_valid = valid

func set_reveal_fraction(value: float) -> void:
	reveal_fraction = clampf(value, 0, 1) if overlay_active and not room_primary else 0.0

func hide_reveal() -> void:
	manual_reveal = false
	reveal_fraction = 0.0
	gesture_epoch += 1 # Also release a latched gesture; it must not reopen next frame.

func show_reveal() -> void:
	if state == "xr" and overlay_active and not room_primary: manual_reveal = true

func set_overlay_above(value: bool) -> void:
	if overlay_above == value: return
	overlay_above = value
	if state == "xr" and overlay_active:
		await stop()
		if not quitting: start()

func request_open_room() -> void:
	if state != "background" or not bridge: return
	if bridge.snapshot().get("activity", "unknown") == "idle":
		auto_scene_blocked = false
		_start_scene()
	else:
		takeover_confirmation_requested.emit(str(bridge.snapshot().get("app", "the current VR app")))

func confirm_takeover() -> void:
	if state == "background" and enabled_intent and bridge:
		auto_scene_blocked = false
		_start_scene(true)

func _process(delta: float) -> void:
	if bridge:
		bridge.poll()
		observation = bridge.snapshot()
		if not bridge.error.is_empty() and state not in ["stopping", "starting"]:
			var message: String = bridge.error
			await stop(true)
			_set_state("desktop", "SteamVR • " + message + " • Enable VR to retry")
			return
		if openvr_active and state == "xr":
			if interface.call("is_runtime_exiting"):
				await stop(true)
				return
			var now := Time.get_ticks_msec()
			if observation.get("activity") == "idle":
				if background_idle_since < 0: background_idle_since = now
			else: background_idle_since = -1
			if background_idle_since >= 0 and now - background_idle_since >= 750:
				await stop(false, true)
				return
		if state == "background":
			var activity: String = observation.get("activity", "unknown")
			var now := Time.get_ticks_msec()
			if activity == "idle":
				if background_idle_since < 0: background_idle_since = now
			else: background_idle_since = -1
			if not auto_scene_blocked and background_idle_since >= 0 and now - background_idle_since >= 750:
				_start_scene()
				return
			if openvr_peek and not openvr_failed and openvr_interface and activity in ["other", "unknown"]:
				_start_openvr_overlay()
				return
			if not auto_scene_blocked:
				var app_name := str(observation.get("app", "Game"))
				var message := "SteamVR • " + ({"idle":"Opening Prim room…", "other":"Tracking alongside " + app_name, "transition":"Waiting for app transition", "unknown":"Checking active app…", "self":"Waiting for scene release"}.get(activity, "Checking active app…") as String)
				if message != detail: _set_state("background", message)
			return
	observe_elapsed += delta
	if monitor and not bridge and observe_elapsed >= 0.2:
		observe_elapsed = 0.0
		var snapshot = JSON.parse_string(monitor.call("snapshot_json"))
		if snapshot is Dictionary: observation = snapshot
	if state == "desktop":
		var activity := str(observation.get("activity", "unknown"))
		var status := "%s • %s" % [observation.get("runtime", "Undetected"), {"idle":"Idle", "other":"Other app: " + str(observation.get("app", "")), "unavailable":"Runtime inactive", "unknown":"Activity unknown"}.get(activity, "Activity unknown")]
		# Preserve actionable start failures until a successful retry.
		if detail.begins_with("Desktop") and detail != "Desktop • " + status:
			_set_state("desktop", "Desktop • " + status)
		return
	if state != "xr": return
	var primary := not overlay_active
	var label := "Prim room • companion overlay unavailable"
	if overlay_active:
		var info: Dictionary = {"main_visible":0, "visibility_serial":0} if openvr_active else interface.call("get_overlay_status")
		var serial := int(info.get("visibility_serial", -1))
		if serial != visibility_serial:
			visibility_serial = serial
			event_elapsed = 0.0
			idle_elapsed = 0.0
		event_elapsed += delta
		var activity := str(observation.get("activity", "unknown"))
		if int(observation.get("age_ms", 9999)) > 1500 or (not openvr_active and str(observation.get("runtime", "")) != "Monado"): activity = "unknown"
		if int(info.get("main_visible", -1)) == 1: activity = "other"
		var key := activity + ":" + str(observation.get("app", ""))
		if key != activity_key:
			activity_key = key
			gesture_epoch += 1
			manual_reveal = false
			reveal_fraction = 0.0
		if activity == "idle" and event_elapsed >= 0.5: idle_elapsed += delta
		else: idle_elapsed = 0.0
		primary = idle_elapsed >= 0.35 and not openvr_active
		if activity == "idle": reveal_fraction = 0.0
		var app_name := str(observation.get("app", ""))
		if app_name.is_empty(): app_name = "Game"
		label = "Prim room" if primary else ("Other app: " + app_name if activity == "other" else "Activity unknown • lift to reveal")
		if not primary: label += " • " + ("Room reveal" if reveal_fraction > 0 else "Tracking")
	if primary != room_primary:
		room_primary = primary
		reveal_fraction = 0.0
		presentation_changed.emit(room_primary, 0.0)
	var amount := (1.0 if room_primary or manual_reveal else reveal_fraction) if head_valid else 0.0
	if not is_equal_approx(amount, presentation_fraction):
		presentation_fraction = amount
		if overlay_active: interface.call("set_overlay_hidden", amount <= 0.001)
		if openvr_active and render_viewport is SubViewport:
			render_viewport.render_target_update_mode = SubViewport.UPDATE_DISABLED if amount <= 0.001 else SubViewport.UPDATE_ALWAYS
		presentation_changed.emit(room_primary, amount)
	var message := "%s • %s" % [runtime_name, label]
	if message != detail: _set_state("xr", message)

func _exit_tree() -> void:
	if openvr_interface and not openvr_interface.is_initialized():
		XRServer.remove_interface(openvr_interface)
