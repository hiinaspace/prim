class_name PrimPlayback
extends Node

# Host revisions order intent; periodic samples describe the host's actual media clock.
# Each process has its own monotonic clock, estimated with four-timestamp probes.
var player
var session
var menu: PrimMenu
var source := ""
var source_kind := "url"
var local_path := ""
var local_files := {}
var generation := 0
var revision := 0
var loaded := false
var desired_paused := true
var sample := {}
var host_offset := 0.0
var best_rtt := INF
var clock_ready := false
var elapsed := 0.0
var correction_elapsed := 0.0
var awaiting_seek := false
var status := "Choose a video to begin."
var last_snapshot_sequence := -1
var snapshot_sequence := 0
var pending_probes := {}
var probe_sequence := 0
var drift_seconds := 0.0

static func now() -> float:
	return Time.get_ticks_usec() / 1000000.0

static func target_position(state: Dictionary, host_now: float) -> float:
	return maxf(0.0, float(state.position) + (maxf(0.0, host_now - float(state.stamp)) if not state.paused else 0.0))

static func valid_snapshot(message: Dictionary) -> bool:
	for key in ["generation", "revision", "sequence", "position", "stamp", "duration"]:
		if not message.get(key) is float and not message.get(key) is int: return false
		if not is_finite(float(message[key])): return false
	return message.get("source") is String and message.source.length() <= 4096 and message.get("kind") in ["url", "file"] and message.get("paused") is bool and message.position >= 0 and message.duration >= 0 and message.generation >= 0 and message.revision >= 0

func _ready() -> void:
	player.file_loaded.connect(file_loaded)
	player.playback_error.connect(func(message):
		loaded = false
		status = "Playback error: " + message)
	player.playback_finished.connect(func():
		desired_paused = true
		status = "Finished")

func authority() -> bool:
	return not session.is_active() or session.is_host()

func request_source(value: String) -> void:
	value = value.strip_edges()
	if value.is_empty() or value.length() > 4096: return
	var remote := value.begins_with("https://") or value.begins_with("http://")
	if not remote:
		if not FileAccess.file_exists(value):
			status = "File not found. Paste a URL or choose an existing local path."
			return
		local_path = ProjectSettings.globalize_path(value)
		local_files[value.get_file()] = local_path
		if not authority() and source_kind == "file":
			load_local(local_path)
			return
	var descriptor := value if remote else value.get_file()
	if authority():
		set_source(descriptor, "url" if remote else "file")
	else:
		send(session.get_host_id(), {"type":"request", "action":"source", "source":descriptor, "kind":"url" if remote else "file"})

func set_source(value: String, kind: String) -> void:
	source = value
	source_kind = kind
	generation += 1
	revision += 1
	desired_paused = false
	sample.clear()
	load_local(source if kind == "url" else local_path)
	broadcast_snapshot()

func load_local(path: String) -> void:
	loaded = false
	awaiting_seek = true
	if path.is_empty():
		status = "This room is watching %s. Open your local copy to synchronize." % source
		return
	status = "Loading…"
	player.pause()
	player.load(path)

func file_loaded() -> void:
	loaded = true
	status = "Ready"
	if authority():
		player.set_paused(desired_paused)
		broadcast_snapshot()
	else:
		correct(true)

func request_action(action: String, value: float = 0.0) -> void:
	if authority(): apply_action(action, value)
	else: send(session.get_host_id(), {"type":"request", "action":action, "value":value})

func apply_action(action: String, value: float) -> void:
	if source.is_empty(): return
	if action == "toggle":
		desired_paused = not desired_paused
		player.set_paused(desired_paused)
	elif action == "seek" and is_finite(value):
		var position := maxf(0.0, float(player.get_playback_position()) + clampf(value, -3600, 3600))
		if player.get_duration() > 0: position = minf(position, player.get_duration())
		player.seek(position)
	else: return
	revision += 1
	# Cached mpv position follows the async seek; publish again after it settles.
	broadcast_snapshot()

func host_changed() -> void:
	clock_ready = false
	best_rtt = INF
	last_snapshot_sequence = -1
	pending_probes.clear()
	if session.is_host():
		sample.clear()
		desired_paused = player.is_paused()
		broadcast_snapshot()
	else:
		probe_clock()
		send(session.get_host_id(), {"type":"hello"})

func left_room() -> void:
	sample.clear()
	clock_ready = false
	best_rtt = INF
	last_snapshot_sequence = -1
	pending_probes.clear()
	desired_paused = player.is_paused()
	if player.has_method("set_playback_speed"): player.set_playback_speed(1.0)

func peer_joined(peer: String) -> void:
	if session.is_host(): send_snapshot(peer)
	elif peer == session.get_host_id():
		probe_clock()
		send(peer, {"type":"hello"})

func send(peer: String, message: Dictionary) -> void:
	if not peer.is_empty(): session.send_control(peer, JSON.stringify(message))

func snapshot() -> Dictionary:
	snapshot_sequence += 1
	return {"type":"snapshot", "sequence":snapshot_sequence, "generation":generation, "revision":revision, "source":source, "kind":source_kind, "position":player.get_playback_position(), "duration":player.get_duration(), "paused":player.is_paused() or not loaded or not player.is_playing(), "stamp":now()}

func send_snapshot(peer: String) -> void:
	if not source.is_empty(): send(peer, snapshot())

func broadcast_snapshot() -> void:
	if session.is_host() and not source.is_empty(): session.broadcast_control(JSON.stringify(snapshot()))

func probe_clock() -> void:
	if session.get_host_id().is_empty() or session.is_host(): return
	probe_sequence += 1
	pending_probes[probe_sequence] = now()
	if pending_probes.size() > 8: pending_probes.erase(pending_probes.keys()[0])
	send(session.get_host_id(), {"type":"clock_ping", "id":probe_sequence})

func message_received(peer: String, message: Dictionary) -> void:
	var received := now()
	match message.get("type"):
		"hello":
			if session.is_host(): send_snapshot(peer)
		"clock_ping":
			if session.is_host() and (message.get("id") is float or message.get("id") is int):
				send(peer, {"type":"clock_pong", "id":message.id, "received":received, "sent":now()})
		"clock_pong":
			if peer != session.get_host_id(): return
			var id := int(message.get("id", -1))
			if not pending_probes.has(id): return
			for key in ["received", "sent"]:
				if not (message.get(key) is float or message.get(key) is int) or not is_finite(float(message[key])): return
			var sent: float = pending_probes[id]
			pending_probes.erase(id)
			var rtt: float = received - sent - (message.sent - message.received)
			if rtt >= 0 and rtt < best_rtt:
				best_rtt = rtt
				host_offset = ((message.received - sent) + (message.sent - received)) * 0.5
				clock_ready = true
				correct(true)
		"request":
			if not session.is_host(): return
			if message.get("action") == "source" and message.get("source") is String and message.get("kind") in ["url", "file"]:
				var value: String = message.source
				if value.length() > 4096 or value.is_empty(): return
				if message.kind == "url" and not (value.begins_with("https://") or value.begins_with("http://")): return
				if message.kind == "file":
					local_path = ""
					value = value.get_file()
				set_source(value, message.kind)
			elif message.get("action") in ["toggle", "seek"]:
				var value: Variant = message.get("value", 0.0)
				if value is float or value is int: apply_action(message.action, value)
		"snapshot":
			if peer != session.get_host_id() or not valid_snapshot(message): return
			if int(message.sequence) <= last_snapshot_sequence: return
			last_snapshot_sequence = int(message.sequence)
			var changed: bool = generation != int(message.generation) or source != message.source or source_kind != message.kind
			var new_revision := revision != int(message.revision)
			generation = int(message.generation)
			revision = int(message.revision)
			source = message.source
			source_kind = message.kind
			desired_paused = message.paused
			sample = message
			if changed:
				if source_kind == "file": local_path = local_files.get(source, "")
				load_local(source if source_kind == "url" else local_path)
			else: correct(new_revision)

func correct(force: bool = false) -> void:
	if authority() or not loaded or sample.is_empty() or not clock_ready: return
	if player.has_method("get_playback_state"):
		var state: Dictionary = player.get_playback_state()
		if state.get("seeking", false) or state.get("loading", false): return
	var target := target_position(sample, now() + host_offset)
	if player.get_duration() > 0: target = minf(target, player.get_duration())
	drift_seconds = target - float(player.get_playback_position())
	if awaiting_seek or (force and absf(drift_seconds) > 0.12) or absf(drift_seconds) > 0.75:
		player.seek(target)
		awaiting_seek = false
		if player.has_method("set_playback_speed"): player.set_playback_speed(1.0)
	elif player.has_method("set_playback_speed"):
		player.set_playback_speed(clampf(1.0 + drift_seconds * 0.08, 0.98, 1.02) if absf(drift_seconds) > 0.04 and not desired_paused else 1.0)
	player.set_paused(desired_paused)

func _process(delta: float) -> void:
	elapsed += delta
	correction_elapsed += delta
	if elapsed >= 1.0:
		elapsed = 0
		if session.is_host(): broadcast_snapshot()
		elif session.is_active(): probe_clock()
	if correction_elapsed >= 0.25:
		correction_elapsed = 0
		correct()
	menu.play_button.text = "Play" if player.is_paused() else "Pause"
	menu.media_status.text = "%s  •  %.1f / %.1f s%s" % [status, player.get_playback_position(), player.get_duration(), "  • sync %+.0f ms" % (drift_seconds * 1000) if clock_ready else ""]
