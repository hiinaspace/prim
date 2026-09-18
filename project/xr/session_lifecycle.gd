## Owns only the OpenXR lifetime. The room, media and microphone outlive it.
extends Node

signal changed(state: String, detail: String)
signal mode_changed(in_vr: bool)
signal presentation_changed(primary_room: bool, amount: float)

var state := "desktop"
var detail := "Desktop • enable VR whenever your headset is ready."
var interface: XRInterface
var quitting := false
var desktop_vsync: int
var render_viewport: Viewport
var monitor: RefCounted
var observation := {"activity": "unknown", "runtime": "Undetected"}
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
	if render_viewport == null: render_viewport = get_viewport()
	if ClassDB.class_exists("PrimXRMonitor"): monitor = ClassDB.instantiate("PrimXRMonitor")
	interface = XRServer.find_interface("OpenXR")
	if interface:
		interface.session_stopping.connect(func(): _runtime_ended.call_deferred(false))
		interface.session_loss_pending.connect(func(): _runtime_ended.call_deferred(true))
		interface.instance_exiting.connect(func(): _runtime_ended.call_deferred(false))
	get_tree().auto_accept_quit = false
	get_window().close_requested.connect(close)

func _set_state(value: String, message: String) -> void:
	state = value
	detail = message
	changed.emit(state, detail)
	print("PRIM_XR state=", state, " detail=", detail)

func toggle() -> void:
	if state == "desktop": start()
	elif state == "xr": stop()

func start() -> void:
	if state != "desktop" or quitting: return
	if not interface or not interface.has_method("request_session_exit"):
		_set_state("desktop", "VR unavailable in this build. Use Prim's patched engine.")
		return
	_set_state("starting", "Starting VR…")
	await get_tree().process_frame
	if state != "starting": return
	if interface.has_method("configure_overlay"):
		interface.call("configure_overlay", OS.get_name() == "Linux" and OS.get_environment("PRIM_XR_SCENE") != "1", 1)
	if not interface.initialize():
		interface.uninitialize()
		_set_state("desktop", "Could not start VR. Check your runtime and headset, then try again.")
		return
	overlay_active = interface.has_method("get_overlay_status") and bool(interface.call("get_overlay_status").get("active", false))
	runtime_name = str(interface.get_system_info().get("XRRuntimeName", "OpenXR"))
	if "monado" in runtime_name.to_lower(): runtime_name = "Monado"
	elif "steam" in runtime_name.to_lower(): runtime_name = "SteamVR"
	if monitor: monitor.call("set_monado_session_active", "monado" in runtime_name.to_lower())
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
	if state == "xr": stop(lost)

func stop(runtime_lost := false) -> void:
	if state == "desktop" or state == "stopping": return
	_set_state("stopping", "Returning to desktop…")
	if interface and interface.is_initialized() and not runtime_lost:
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
	DisplayServer.window_set_vsync_mode(desktop_vsync)
	var device := RenderingServer.get_rendering_device()
	if device:
		for _frame in range(device.get_frame_delay() + 1):
			await RenderingServer.frame_post_draw
	RenderingServer.force_sync()
	if interface: interface.uninitialize()
	_set_state("desktop", "VR runtime disconnected. You are still in Prim." if runtime_lost else "Desktop • enable VR whenever your headset is ready.")

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
	reveal_fraction = 0.0

func _process(delta: float) -> void:
	observe_elapsed += delta
	if monitor and observe_elapsed >= 0.2:
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
		var info: Dictionary = interface.call("get_overlay_status")
		var serial := int(info.get("visibility_serial", -1))
		if serial != visibility_serial:
			visibility_serial = serial
			event_elapsed = 0.0
			idle_elapsed = 0.0
		event_elapsed += delta
		var activity := str(observation.get("activity", "unknown"))
		if int(observation.get("age_ms", 9999)) > 1500 or str(observation.get("runtime", "")) != "Monado": activity = "unknown"
		if int(info.get("main_visible", -1)) == 1: activity = "other"
		var key := activity + ":" + str(observation.get("app", ""))
		if key != activity_key:
			activity_key = key
			gesture_epoch += 1
			reveal_fraction = 0.0
		if activity == "idle" and event_elapsed >= 0.5: idle_elapsed += delta
		else: idle_elapsed = 0.0
		primary = idle_elapsed >= 0.35
		if activity != "other": reveal_fraction = 0.0
		var app_name := str(observation.get("app", ""))
		if app_name.is_empty(): app_name = "Game"
		label = "Prim room" if primary else ("Other app: " + app_name if activity == "other" else "Activity unknown")
		if not primary: label += " • " + ("Room reveal" if reveal_fraction > 0 else "Tracking")
	if primary != room_primary:
		room_primary = primary
		reveal_fraction = 0.0
		presentation_changed.emit(room_primary, 0.0)
	var amount := (1.0 if room_primary else reveal_fraction) if head_valid else 0.0
	if not is_equal_approx(amount, presentation_fraction):
		presentation_fraction = amount
		if overlay_active: interface.call("set_overlay_hidden", amount <= 0.001)
		presentation_changed.emit(room_primary, amount)
	var message := "%s • %s" % [runtime_name, label]
	if message != detail: _set_state("xr", message)
