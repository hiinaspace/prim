extends SceneTree
var app
var role := OS.get_environment("PRIM_TEST_ROLE")
var output := OS.get_environment("PRIM_TEST_OUTPUT")
var failures: Array[String] = []
var peers := {}
var reports := {}
var done := false
func approve_test_relays() -> void:
	# Explicit opt-in for synthetic fixture bytes in normal-network Wine tests.
	if not app or OS.get_environment("PRIM_TEST_ALLOW_RELAY") != "1": return
	var state: Dictionary = JSON.parse_string(app.session.media_state())
	var publication: Variant = state.get("publication")
	if not publication is Dictionary: return
	for viewer in state.get("viewers", {}).values():
		if viewer.get("consent") == "pending":
			app.session.allow_media_relay(publication.id, viewer.peer, true)
func _initialize() -> void: call_deferred("run")
func check(ok: bool, label: String) -> void:
	print("SHARING_CHECK ", label, ": ", ok)
	if not ok: failures.append(label)
func wait_for(predicate: Callable, seconds: float = 15) -> bool:
	var end := Time.get_ticks_msec() + seconds * 1000
	while not predicate.call() and Time.get_ticks_msec() < end: await process_frame
	return predicate.call()
func send(peer: String, action: String) -> void:
	app.session.send_control(peer, JSON.stringify({"type":"sharing_test", "action":action}))
func receive(peer: String, raw: String) -> void:
	var msg: Variant = JSON.parse_string(raw)
	if not msg is Dictionary: return
	if msg.get("type") == "sharing_role": peers[msg.role] = peer
	if msg.get("type") == "sharing_report": reports[peer] = msg
	if msg.get("type") != "sharing_test": return
	match msg.action:
		"share":
			app.menu.choose_source(OS.get_environment("PRIM_TEST_HOST_FILE"))
			check(app.menu.file_choice.visible, "movie picker prompts before sharing")
			app.menu.share_button.pressed.emit()
		"invalid": app.playback.share_file("/no/such/file")
		"stop": app.playback.stop_share()
		"off": app.player.set_subtitle_track(-1)
		"report":
			app.session.send_control(peer, JSON.stringify({"type":"sharing_report", "source":app.playback.source, "loaded":app.playback.loaded, "sid":app.player.get_subtitle_track(), "host":app.session.get_host_id(), "hosted":JSON.parse_string(app.session.media_state()).get("hosted", false), "peers":app.avatars.keys()}))
		"leave":
			app.toggle_connection()
			await create_timer(0.5).timeout
			finish()
		"finish": finish()
func converge() -> void:
	# Relay setup and the late viewer's initial decode can exceed one second.
	# Poll actual convergence with a deadline instead of sampling a fixed delay.
	var deadline := Time.get_ticks_msec() + 15000
	while Time.get_ticks_msec() < deadline:
		reports.clear()
		for peer in app.avatars: send(peer, "report")
		await wait_for(func(): return reports.size() == app.avatars.size(), 2)
		var converged: bool = reports.size() == app.avatars.size()
		for report in reports.values():
			converged = converged and report.source == app.playback.source and report.loaded == app.playback.loaded
		if converged: break
		await create_timer(0.25).timeout
	check(reports.size() == app.avatars.size(), "all viewers report")
	for report in reports.values():
		if report.source != app.playback.source or report.loaded != app.playback.loaded:
			print("SHARING_STATE ", JSON.stringify({"host_source":app.playback.source, "host_loaded":app.playback.loaded, "viewer":report}))
		check(report.source == app.playback.source and report.loaded == app.playback.loaded, "room source converges")
		check(report.host == app.session.get_local_id(), "provider takeover preserves room host")
func share_from(peer: String) -> void:
	var generation: int = app.playback.generation
	send(peer, "share")
	check(await wait_for(func(): return app.playback.generation > generation and app.playback.loaded and app.playback.current_descriptor().get("owner") == peer), "non-host takeover completes")
	await converge()
func run() -> void:
	app = load("res://main.tscn").instantiate()
	root.add_child(app)
	app.get_tree().process_frame.connect(approve_test_relays)
	app.set_process_input(false)
	app.session.network_error.connect(func(message): print("SHARING_NETWORK_ERROR ", message))
	app.session.message_received.connect(receive)
	app.session.endpoint_ready.connect(func():
		if role == "host": FileAccess.open(output + ".endpoint", FileAccess.WRITE).store_string(app.session.get_endpoint_info()))
	if role == "client1": await create_timer(8).timeout
	app.toggle_connection()
	check(await wait_for(func(): return app.avatars.size() >= 1, 40), "room connected")
	if role != "host":
		app.session.send_control(app.session.get_host_id(), JSON.stringify({"type":"sharing_role", "role":role}))
		await create_timer(75).timeout
		if not done: check(false,"host completed"); finish()
		return
	check(await wait_for(func(): return peers.has("client0")), "initial provider identified")
	if not peers.has("client0"): finish(); return
	send(peers.client0, "share")
	check(await wait_for(func(): return app.playback.loaded and app.playback.source_kind == "peer_file"), "file starts before late join")
	check(await wait_for(func(): return peers.size() == 2), "late viewer joined")
	check(await wait_for(func(): return app.avatars.size() == 2), "three-person mesh formed")
	await converge()
	if peers.size() != 2: finish(); return
	await share_from(peers.client0)
	var first: Dictionary = app.playback.current_descriptor()
	check(await wait_for(func(): return app.player.get_subtitle_tracks().size() > 0), "shared MKV exposes embedded subtitles")
	if app.player.get_subtitle_tracks().size() > 0:
		var track: int = app.player.get_subtitle_tracks()[0].id
		app.player.set_subtitle_track(track)
		send(peers.client0, "off")
		await converge()
		check(app.player.get_subtitle_track() == track and reports[peers.client0].sid == -1, "subtitle selection is local")
	await share_from(peers.client1)
	var second: String = app.playback.source
	app.playback.message_received(peers.client0, {"type":"file_stopped", "id":first.id})
	app.playback.message_received(peers.client0, {"type":"file_ready", "id":first.id})
	check(app.playback.source == second, "stale stop and ready cannot replace newer video")
	check(not reports[peers.client0].hosted, "replacement revokes old provider")
	var forged: Dictionary = app.playback.current_descriptor().duplicate()
	forged.owner = peers.client0
	app.playback.message_received(peers.client1, {"type":"file_offer", "source":JSON.stringify(forged)})
	check(app.playback.pending_offer.is_empty(), "provider cannot claim another member's identity")
	send(peers.client0, "invalid")
	await create_timer(0.4).timeout
	check(app.playback.source == second, "invalid preparation preserves room video")
	send(peers.client1, "stop")
	check(await wait_for(func(): return app.playback.source.is_empty()), "provider can stop its own share")
	await converge()
	app.playback.request_source(OS.get_environment("PRIM_TEST_MEDIA"))
	check(await wait_for(func(): return app.playback.loaded), "URL playback after shared file")
	await converge()
	var generation: int = app.playback.generation
	send(peers.client0, "share")
	send(peers.client1, "share")
	check(await wait_for(func(): return app.playback.generation > generation and app.playback.loaded and app.playback.pending_offer.is_empty()), "competing offers produce a committed source")
	await converge()
	var owner: String = app.playback.current_descriptor().owner
	for peer in app.avatars:
		check(reports[peer].hosted == (peer == owner), "only committed provider remains published")
	app.playback.stop_share()
	check(app.playback.source.is_empty(), "host can stop client publication")
	await converge()
	await share_from(peers.client0)
	send(peers.client0, "leave")
	check(await wait_for(func(): return app.playback.source.is_empty() and app.avatars.size() == 1), "provider departure clears room video")
	await converge()
	for peer in app.avatars: send(peer, "finish")
	finish()
func finish() -> void:
	if done: return
	done = true
	FileAccess.open(output + ".json", FileAccess.WRITE).store_string(JSON.stringify({"role":role,"failures":failures}))
	print("SHARING_RESULT ", JSON.stringify(failures))
	quit(0 if failures.is_empty() else 1)
