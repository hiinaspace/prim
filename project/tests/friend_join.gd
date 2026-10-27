extends SceneTree
# One role of tools/test-friend-join.py: host (Friends instance), guest (listed
# friend, presses Join) or intruder (not listed, presses Join and is refused).
# Roles exchange ids and finish markers through files in PRIM_TEST_DIR.
var role := OS.get_environment("PRIM_TEST_ROLE")
var dir := OS.get_environment("PRIM_TEST_DIR")
var app
var failures: Array[String] = []
var finished := false

func _initialize() -> void:
	call_deferred("run")

func check(condition: bool, message: String) -> void:
	print("CHECK ", role, " ", message, ": ", condition)
	if not condition: failures.append(message)

func wait_for(predicate: Callable, seconds := 30.0) -> bool:
	var deadline := Time.get_ticks_msec() + int(seconds * 1000)
	while not predicate.call() and Time.get_ticks_msec() < deadline:
		await process_frame
	return predicate.call()

func file_text(name: String) -> String:
	var path := dir + "/" + name
	return FileAccess.get_file_as_string(path) if FileAccess.file_exists(path) else ""

func write(name: String, text: String) -> void:
	var file := FileAccess.open(dir + "/" + name + ".tmp", FileAccess.WRITE)
	file.store_string(text)
	file.close()
	DirAccess.rename_absolute(dir + "/" + name + ".tmp", dir + "/" + name)

func press(parent: Node, text: String) -> bool:
	for node in parent.find_children("*", "Button", true, false):
		if node.text == text and not node.disabled:
			node.pressed.emit()
			return true
	return false

# Synthetic microphone: keeps the shared sender fed so voice crosses the wire.
func pump_voice() -> void:
	var chunk := PackedFloat32Array()
	chunk.resize(960)
	var phase := 0
	while not finished:
		for i in range(960): chunk[i] = 0.4 * sin(TAU * 440.0 * float(phase + i) / 48000.0)
		phase += 960
		app.sender.push_pcm_mono(chunk)
		await create_timer(0.02).timeout

func join_host_by_friend_list() -> void:
	var info: Variant = JSON.parse_string(file_text("host.endpoint"))
	# Local-only runs cannot resolve a bare id; main.gd passes this full address instead.
	OS.set_environment("PRIM_TEST_HOST", file_text("host.endpoint"))
	app.friends.add_by_code(info.id, "Host")
	app.menu.select_tab("Friends")
	await process_frame
	await process_frame
	check(press(app.menu.friend_rows, "Join"), "Join button next to the host's name is pressable")

func run() -> void:
	root.gui_embed_subwindows = true
	app = load("res://main.tscn").instantiate()
	root.add_child(app)
	await create_timer(1.0).timeout
	write(role + ".id", app.identity_id)
	if role == "host":
		check(await wait_for(func(): return not file_text("guest.id").is_empty()), "guest id available")
		app.friends.add_by_code(file_text("guest.id"), "Guest")
		check(press(app.menu.tabs.get_child(0), "Host Friends instance"), "Host Friends instance pressed")
		check(await wait_for(func(): return app.session.get_endpoint_info() != ""), "endpoint ready")
		write("host.endpoint", app.session.get_endpoint_info())
		var guest := file_text("guest.id")
		check(await wait_for(func(): return app.avatars.has(guest)), "listed friend joined")
		pump_voice()
		check(await wait_for(func(): return app.avatars.has(guest) and app.avatars[guest].last_sequence > 10), "guest pose reaches host")
		check(await wait_for(func(): return app.session.receive_stream(guest).get_stats().get("non_silent_output_frames", 0) > 10000), "guest voice decoded at host")
		check(await wait_for(func(): return not file_text("intruder.done").is_empty(), 45.0), "intruder attempt finished")
		var intruder := file_text("intruder.id")
		check(app.avatars.size() == 1 and not app.avatars.has(intruder), "unlisted key never appears in the room")
		check(app.session.is_host() and app.hosting_mode == "friends", "still hosting")
		write("host.done", "1")
	elif role == "guest":
		check(await wait_for(func(): return not file_text("host.endpoint").is_empty()), "host endpoint published")
		await join_host_by_friend_list()
		var host_id: String = JSON.parse_string(file_text("host.endpoint")).id
		check(await wait_for(func(): return app.avatars.has(host_id)), "joined the Friends instance")
		check(app.session.get_host_id() == host_id and not app.session.is_host(), "host is the friend who hosts")
		pump_voice()
		check(await wait_for(func(): return app.avatars[host_id].last_sequence > 10), "host pose reaches guest")
		check(await wait_for(func(): return app.session.receive_stream(host_id).get_stats().get("non_silent_output_frames", 0) > 10000), "host voice decoded at guest")
		check(await wait_for(func(): return not file_text("host.done").is_empty(), 60.0), "host finished")
	else:
		check(await wait_for(func(): return not file_text("host.endpoint").is_empty()), "host endpoint published")
		await join_host_by_friend_list()
		check(await wait_for(func(): return not app.session.is_active(), 20.0), "refused join ends the session")
		check(app.status_text.contains("not admitted"), "refusal reason is shown: " + app.status_text)
		check(app.avatars.is_empty(), "intruder has no peers")
		write("intruder.done", "1")
	finished = true
	write(role + ".json", JSON.stringify({"failures": failures}))
	quit(1 if not failures.is_empty() else 0)
