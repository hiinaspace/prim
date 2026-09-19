extends SceneTree
func _initialize(): run.call_deferred()
func run():
	var monitor = ClassDB.instantiate("PrimXRMonitor")
	monitor.set_monado_session_active(true)
	for _i in range(100):
		var sample: Dictionary = JSON.parse_string(monitor.snapshot_json())
		if sample.get("activity") == "idle":
			var selected := str(sample.get("status_library", ""))
			var ok := selected == OS.get_environment("PRIM_EXPECT_STATUS_LIBRARY")
			print("MONADO_STATUS_FIXTURE ",ok)
			monitor = null
			quit(0 if ok else 1)
			return
		await create_timer(0.05).timeout
	print("MONADO_STATUS_FIXTURE timed out")
	monitor = null
	quit(1)
