extends SceneTree
class FakeRuntime extends XRInterfaceExtension:
	var visibility := -1
	var serial := 0
	var hidden := true
	func get_overlay_status() -> Dictionary: return {"main_visible":visibility, "visibility_serial":serial}
	func set_overlay_hidden(value: bool) -> void: hidden = value
var failures := []
func _initialize() -> void: run.call_deferred()
func check(value: bool, message: String) -> void:
	print("CHECK ", message, ": ", value)
	if not value: failures.append(message)
func run() -> void:
	var life = preload("res://xr/session_lifecycle.gd").new()
	root.add_child(life)
	life.set_process(false)
	life.monitor = null
	var runtime := FakeRuntime.new()
	life.interface = runtime
	life.state = "xr"
	life.overlay_active = true
	life.head_valid = true
	life.observation = {"runtime":"Monado", "activity":"idle", "age_ms":0}
	life._process(0.1)
	check(not life.room_primary, "initial unknown event state does not instantly grant controls")
	life._process(0.5)
	check(life.room_primary and not runtime.hidden, "confirmed idle shows room without promoting scene role")
	runtime.visibility = 1
	runtime.serial += 1
	life._process(0.01)
	check(not life.room_primary and runtime.hidden, "main-visible event gates controls before stale idle poll")
	life.observation = {"runtime":"Monado", "activity":"other", "app":"VTOL", "age_ms":0}
	life._process(0.1)
	life.set_reveal_fraction(1)
	life._process(0.1)
	check(not life.room_primary and not runtime.hidden, "latched reveal cannot grant gameplay actions")
	runtime.visibility = 0
	runtime.serial += 1
	life._process(1.0)
	check(not life.room_primary, "visibility loss is not game exit")
	life.observation["age_ms"] = 2000
	life._process(0.1)
	check(not life.room_primary and runtime.hidden and life.reveal_fraction == 0, "stale IPC cancels reveal and fails closed")
	life.set_reveal_fraction(1)
	life._process(0.1)
	check(not runtime.hidden and not life.room_primary, "unknown IPC still permits deliberate gesture reveal without scene controls")
	life.hide_reveal()
	life.show_reveal()
	life._process(0.1)
	check(not runtime.hidden, "desktop reveal works without status IPC")
	life.hide_reveal()
	life.observation = {"runtime":"Monado", "activity":"idle", "age_ms":0}
	life._process(0.5)
	check(life.room_primary, "confirmed exit automatically restores room")
	life.head_valid = false
	life._process(0.1)
	check(runtime.hidden and life.presentation_fraction == 0, "invalid head hides projection")
	life.head_valid = true
	life._process(0.1)
	check(not runtime.hidden, "tracking restoration reveals standalone room")
	life.observation = {"runtime":"Monado", "activity":"other", "app":"Mech", "age_ms":0}
	life._process(0.1)
	life.set_reveal_fraction(0.5)
	life._process(0.1)
	check(not runtime.hidden, "partial reveal renders")
	life.hide_reveal()
	life._process(0.1)
	check(runtime.hidden, "desktop escape hides reveal")
	life.state = "desktop"
	life._process(1)
	check(not life.room_primary, "disabled VR does not auto-enter on observation")
	life.free()
	print("XR_POLICY_RESULT ", JSON.stringify(failures))
	quit(0 if failures.is_empty() else 1)
