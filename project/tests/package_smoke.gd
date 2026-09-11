extends SceneTree
var app
var failures: Array[String] = []
func _initialize() -> void: call_deferred("run")
func check(condition: bool, message: String) -> void:
	print("CHECK ", message, ": ", condition)
	if not condition: failures.append(message)
func run() -> void:
	app = load("res://main.tscn").instantiate()
	root.add_child(app)
	print("PACKAGE_READY")
	var deadline := Time.get_ticks_msec() + 15000
	while not app.playback.loaded and Time.get_ticks_msec() < deadline: await process_frame
	check(app.playback.loaded, "packaged media loads")
	app.menu.visible = false
	await create_timer(2.0).timeout
	check(app.player.get_playback_position() > 1.0, "packaged playback advances")
	check(app.player.get_video_width() > 0, "video dimensions available")
	var first := hash(root.get_texture().get_image().get_data())
	await create_timer(0.7).timeout
	var second := hash(root.get_texture().get_image().get_data())
	check(first != second, "rendered video changes")
	check(not app.player.set_playback_speed(NAN), "nonfinite speed rejected")
	check(app.player.set_playback_speed(1.02), "small tempo correction accepted")
	await create_timer(0.3).timeout
	check(absf(app.player.get_playback_state().speed - 1.02) < 0.001, "tempo observation updates")
	app.player.set_playback_speed(1.0)
	app.player.pause()
	await create_timer(0.3).timeout
	check(app.player.get_playback_state().paused, "pause observation updates")
	var output := OS.get_environment("PRIM_TEST_OUTPUT")
	if not output.is_empty(): root.get_texture().get_image().save_png(output + ".png")
	print("PACKAGE_RESULT ", JSON.stringify(failures))
	quit(0 if failures.is_empty() else 1)
