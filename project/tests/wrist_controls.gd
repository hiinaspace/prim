extends SceneTree
var failures := []
func check(ok: bool, label: String):
	print("CHECK ",label,": ",ok)
	if not ok: failures.append(label)
func _initialize():
	var watch = preload("res://xr/wrist_controls.gd").new()
	var fired := -1
	for i in range(6): fired = watch.update_target(0.1, 0)
	check(fired == -1, "brief glance does not toggle")
	check(watch.update_target(0.1, 0) == 0, "complete dwell toggles mic once")
	for i in range(20): check(watch.update_target(0.1, 0) == -1, "held gaze cannot repeat")
	check(watch.update_target(1, 3) == -1, "switching wrists without looking away cannot double toggle")
	watch.update_target(0.2, -1)
	check(watch.update_target(2, 1) == -1, "stalled frame cannot complete dwell")
	watch.update_target(0.1, -1)
	check(watch.elapsed == 0, "lost target cancels partial dwell")
	for i in range(7): fired = watch.update_target(0.1, 3)
	check(fired == 3, "look away rearms deafen on other wrist")
	watch.free()
	print("WRIST_RESULT ",JSON.stringify(failures))
	quit(0 if failures.is_empty() else 1)
