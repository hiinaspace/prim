class_name PrimPlayback
extends Node

# Host revisions order intent; periodic samples describe the host's actual media clock.
# Each process has its own monotonic clock, estimated with four-timestamp probes.
var player
var session
var menu: PrimMenu
var source := ""
var source_kind := "url"
# Local-only playback leaves room intent intact. A host advances a separate room clock.
var local_only := false
var local_only_path := ""
var local_room_clock := {}
var resume_sample := {}
var media_live := false
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
var was_buffering := false
var status := "Choose a video to begin."
var last_snapshot_sequence := -1
var snapshot_sequence := 0
var pending_probes := {}
var probe_sequence := 0
var drift_seconds := 0.0
var last_source_report := ""
var pending_share_path := ""
var offered_id := ""
var published_paths := {}
var pending_offer := {}
var offer_sent_at := 0.0
var waiting_peer_file := false
var media_elapsed := 0.0

static func now() -> float:
	return Time.get_ticks_usec() / 1000000.0

static func target_position(state: Dictionary, host_now: float) -> float:
	return maxf(0.0, float(state.position) + (maxf(0.0, host_now - float(state.stamp)) if not state.paused else 0.0))

static func is_remote_source(value: String) -> bool:
	for scheme in ["http://", "https://", "rtsp://", "rtsps://", "rtmp://", "rtmps://"]:
		if value.begins_with(scheme): return true
	return false

static func live_protocol(value: String) -> bool:
	return value.begins_with("rtsp://") or value.begins_with("rtsps://") or value.begins_with("rtmp://") or value.begins_with("rtmps://")

static func valid_snapshot(message: Dictionary) -> bool:
	if message.has("live") and not message.live is bool: return false
	for key in ["generation", "revision", "sequence", "position", "stamp", "duration"]:
		if not message.get(key) is float and not message.get(key) is int: return false
		if not is_finite(float(message[key])): return false
	return message.get("source") is String and message.source.length() <= 4096 and message.get("kind") in ["url", "file", "peer_file"] and message.get("paused") is bool and message.position >= 0 and message.duration >= 0 and message.generation >= 0 and message.revision >= 0

func _ready() -> void:
	menu.connection_active = session.is_active
	menu.share_file_requested.connect(share_file)
	menu.local_file_requested.connect(play_local_only)
	menu.room_playback_requested.connect(return_to_room)
	menu.stop_share_requested.connect(stop_share)
	menu.relay_decided.connect(func(id, peer, allow): session.allow_media_relay(id, peer, allow))
	menu.media_limit_changed.connect(func(mbps): session.set_media_upload_limit(mbps))
	session.peer_disconnected.connect(peer_left)
	player.file_loaded.connect(file_loaded)
	player.playback_error.connect(func(message):
		loaded = false
		status = "Shared file playback error." if source_kind == "peer_file" else "Playback error: " + message)
	player.playback_finished.connect(func():
		if not local_only: desired_paused = true
		status = "Stream ended — reconnect when the broadcaster returns." if media_live else "Finished")

func authority() -> bool:
	return not session.is_active() or session.is_host()

func request_source(value: String) -> void:
	value = value.strip_edges()
	if value.is_empty() or value.length() > 4096: return
	if not is_remote_source(value):
		play_local_only(value)
		return
	if authority():
		set_source(value, "url")
	else:
		if local_only: return_to_room()
		send(session.get_host_id(), {"type":"request", "action":"source", "source":value, "kind":"url"})

func play_local_only(path: String) -> void:
	path = PrimMenu.normalize_file_path(path)
	if path.is_empty() or not FileAccess.file_exists(path):
		status = "Choose an existing local video file."
		return
	if not session.is_active():
		local_path = ProjectSettings.globalize_path(path)
		local_files[path.get_file()] = local_path
		set_source(path.get_file(), "file")
		return
	if not local_only: local_room_clock = snapshot() if authority() else sample.duplicate(true)
	local_only = true
	local_only_path = ProjectSettings.globalize_path(path)
	waiting_peer_file = false
	session.stop_receiving_file()
	load_local(local_only_path)

func return_to_room() -> void:
	if not local_only: return
	resume_sample = local_room_clock.duplicate(true) if authority() else {}
	local_only = false
	local_only_path = ""
	local_room_clock.clear()
	if source.is_empty(): clear_local_media()
	elif source_kind == "peer_file": open_peer_file()
	else: load_local(source if source_kind == "url" else local_path)

func stream_mode() -> bool:
	# Live demuxers may expose a growing duration or only seek inside their cache.
	if local_only: return false
	if live_protocol(source) or sample.get("live", false): return true
	if loaded:
		var state: Dictionary = player.get_playback_state() if player.has_method("get_playback_state") else {}
		media_live = player.get_duration() <= 0 or not state.get("seekable", true) or state.get("partially_seekable", false)
	return media_live

func room_clock() -> Dictionary:
	return advance_room_clock(local_room_clock)

func advance_room_clock(clock: Dictionary) -> Dictionary:
	var state := clock.duplicate(true)
	if state.is_empty(): return state
	state.position = target_position(state, now())
	if state.duration > 0 and state.position >= state.duration:
		state.position = state.duration
		state.paused = true
	state.stamp = now()
	return state

func set_source(value: String, kind: String) -> void:
	local_only = false
	local_only_path = ""
	local_room_clock.clear()
	resume_sample.clear()
	cancel_pending_offer()
	if kind != "peer_file":
		reset_local_offer()
		published_paths.clear()
		session.stop_sharing()
		pending_share_path = ""
		waiting_peer_file = false
	source = value
	source_kind = kind
	generation += 1
	revision += 1
	desired_paused = false
	sample.clear()
	if kind == "peer_file": open_peer_file()
	else: load_local(source if kind == "url" else local_path)
	broadcast_snapshot()

func load_local(path: String) -> void:
	media_live = live_protocol(path)
	player.set_playback_speed(1.0)
	was_buffering = false
	show_source(path)
	loaded = false
	awaiting_seek = true
	player.pause()
	if path.is_empty():
		player.stop()
		status = "This room is watching %s. Open your local copy to synchronize." % source
		return
	status = "Loading…"
	player.pause()
	player.load(path)

func file_loaded() -> void:
	if local_only:
		loaded = true
		player.set_paused(false)
		status = "Playing only here"
		return
	if source.is_empty():
		player.stop()
		return
	loaded = true
	status = "Ready"
	if stream_mode():
		resume_sample.clear()
		awaiting_seek = false
		player.set_paused(false)
		broadcast_snapshot()
		return
	if authority():
		if not resume_sample.is_empty():
			var target := target_position(resume_sample, now())
			if player.get_duration() > 0: target = minf(target, player.get_duration())
			player.seek(target)
			desired_paused = resume_sample.paused
			# Publish the room clock until the asynchronous restore seek settles.
		player.set_paused(desired_paused)
		broadcast_snapshot()
	else:
		correct(true)

func request_action(action: String, value: float = 0.0) -> void:
	if local_only:
		if action == "toggle": player.set_paused(not player.is_paused())
		elif action in ["seek", "seek_to"] and is_finite(value) and player.get_duration() > 0:
			player.seek(clampf(value if action == "seek_to" else player.get_playback_position() + value, 0, player.get_duration()))
		return
	if stream_mode():
		if action == "toggle":
			if source_kind == "peer_file": open_peer_file()
			else: load_local(source if source_kind == "url" else local_path)
		return
	if authority(): apply_action(action, value)
	else: send(session.get_host_id(), {"type":"request", "action":action, "value":value})

func apply_action(action: String, value: float) -> void:
	if source.is_empty(): return
	if local_only or (not resume_sample.is_empty() and not loaded):
		var state := room_clock() if local_only else advance_room_clock(resume_sample)
		if state.is_empty() or state.get("live", false): return
		if action == "toggle": state.paused = not state.paused
		elif action in ["seek", "seek_to"] and is_finite(value):
			state.position = maxf(0, value if action == "seek_to" else state.position + clampf(value, -3600, 3600))
			if state.duration > 0: state.position = minf(state.position, state.duration)
		else: return
		if local_only: local_room_clock = state
		else: resume_sample = state
		desired_paused = state.paused
		revision += 1
		broadcast_snapshot()
		return
	if stream_mode(): return
	resume_sample.clear()
	if action == "toggle":
		desired_paused = not desired_paused
		player.set_paused(desired_paused)
	elif action in ["seek", "seek_to"] and is_finite(value):
		var position := maxf(0.0, value if action == "seek_to" else float(player.get_playback_position()) + clampf(value, -3600, 3600))
		if player.get_duration() > 0: position = minf(position, player.get_duration())
		player.seek(position)
	else: return
	revision += 1
	# Cached mpv position follows the async seek; publish again after it settles.
	broadcast_snapshot()

func host_changed() -> void:
	cancel_pending_offer()
	session.cancel_file_offer()
	offered_id = ""
	pending_share_path = ""
	session.set_media_upload_limit(int(menu.upload_limit.value))
	clock_ready = false
	best_rtt = INF
	last_snapshot_sequence = -1
	pending_probes.clear()
	if session.is_host():
		if local_only and not sample.is_empty():
			local_room_clock = sample.duplicate(true)
			local_room_clock.stamp = float(local_room_clock.stamp) - host_offset
		sample.clear()
		desired_paused = local_room_clock.get("paused", true) if local_only else player.is_paused()
		broadcast_snapshot()
	else:
		probe_clock()
		send(session.get_host_id(), {"type":"hello"})

func left_room() -> void:
	if local_only:
		source = local_only_path.get_file()
		source_kind = "file"
		local_path = local_only_path
		local_only = false
		local_only_path = ""
		local_room_clock.clear()
	pending_offer.clear()
	reset_local_offer()
	published_paths.clear()
	pending_share_path = ""
	waiting_peer_file = false
	if source_kind == "peer_file":
		source = ""
		clear_local_media()
		status = "Host stopped sharing."
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
	var state: Dictionary
	if local_only:
		state = room_clock()
	elif not resume_sample.is_empty():
		state = advance_room_clock(resume_sample)
	else:
		state = {"position":0.0 if source.is_empty() else player.get_playback_position(), "duration":0.0 if source.is_empty() else player.get_duration(), "paused":source.is_empty() or player.is_paused() or not loaded or not player.is_playing(), "stamp":now(), "live":stream_mode()}
	state.merge({"position":0.0, "duration":0.0, "paused":true, "stamp":now(), "live":false}, false)
	state.merge({"type":"snapshot", "sequence":snapshot_sequence, "generation":generation, "revision":revision, "source":source, "kind":source_kind}, true)
	return state

func send_snapshot(peer: String) -> void:
	send(peer, snapshot())

func broadcast_snapshot() -> void:
	if session.is_host(): session.broadcast_control(JSON.stringify(snapshot()))

func probe_clock() -> void:
	if session.get_host_id().is_empty() or session.is_host(): return
	probe_sequence += 1
	pending_probes[probe_sequence] = now()
	if pending_probes.size() > 8: pending_probes.erase(pending_probes.keys()[0])
	send(session.get_host_id(), {"type":"clock_ping", "id":probe_sequence})

func message_received(peer: String, message: Dictionary) -> void:
	var received := now()
	match message.get("type"):
		"file_offer":
			if session.is_host() and message.get("source") is String:
				accept_offer(peer, message.source)
		"file_activate":
			if peer != session.get_host_id() or offered_id.is_empty() or message.get("id") != offered_id: return
			if session.publish_file(offered_id):
				published_paths[offered_id] = pending_share_path
				send(peer, {"type":"file_ready", "id":offered_id})
			else: send(peer, {"type":"file_stopped", "id":offered_id})
		"file_ready":
			if not session.is_host() or pending_offer.get("peer") != peer or pending_offer.get("id") != message.get("id"): return
			var descriptor: String = pending_offer.source
			pending_offer.clear()
			set_source(descriptor, "peer_file")
		"file_abort":
			if peer != session.get_host_id() or not message.get("id") is String: return
			var id: String = message.id
			if current_descriptor().get("id") == id: return
			session.stop_publishing_file(id)
			published_paths.erase(id)
			if offered_id == id: reset_local_offer()
		"file_stopped":
			if not session.is_host() or not message.get("id") is String: return
			if pending_offer.get("peer") == peer and pending_offer.get("id") == message.id: cancel_pending_offer()
			var descriptor := current_descriptor()
			if descriptor.get("owner") == peer and descriptor.get("id") == message.id: stop_share()
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
				if message.kind == "url" and not is_remote_source(value): return
				if message.kind == "file":
					local_path = ""
					value = value.get_file()
				set_source(value, message.kind)
			elif message.get("action") in ["toggle", "seek", "seek_to"]:
				var value: Variant = message.get("value", 0.0)
				if value is float or value is int: apply_action(message.action, value)
		"snapshot":
			if peer != session.get_host_id() or not valid_snapshot(message): return
			if message.kind == "peer_file" and not message.source.is_empty():
				var descriptor: Variant = JSON.parse_string(message.source)
				if not descriptor is Dictionary or not descriptor.get("owner") is String or not session.validate_file_offer(descriptor.owner, message.source): return
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
			if local_only:
				local_room_clock = message.duplicate(true)
				if not changed: return
				local_only = false
				local_only_path = ""
				local_room_clock.clear()
			if source.is_empty():
				if changed or loaded: clear_local_media()
				return
			if changed:
				waiting_peer_file = false
				if source_kind == "peer_file":
					open_peer_file()
				else:
					session.stop_receiving_file()
					revoke_unused_publication()
					if source_kind == "file": local_path = local_files.get(source, "")
					load_local(source if source_kind == "url" else local_path)
			else: correct(new_revision)

func correct(force: bool = false) -> void:
	if local_only or authority() or not loaded: return
	if stream_mode():
		awaiting_seek = false
		drift_seconds = 0
		player.set_playback_speed(1.0)
		player.set_paused(false)
		return
	if sample.is_empty() or not clock_ready: return
	if player.has_method("get_playback_state"):
		var state: Dictionary = player.get_playback_state()
		if state.get("buffering", false):
			was_buffering = true
			return
		if state.get("seeking", false) or state.get("loading", false): return
		if was_buffering:
			# Recover room time immediately after a stalled download.
			force = true
			was_buffering = false
	var target := target_position(sample, now() + host_offset)
	if player.get_duration() > 0: target = minf(target, player.get_duration())
	drift_seconds = target - float(player.get_playback_position())
	# Range downloads can delay an asynchronous seek even without mpv reporting
	# buffering. Do not spend tens of seconds speed-correcting that startup lag.
	var seek_threshold := 0.25 if source_kind == "peer_file" else 0.75
	if awaiting_seek or (force and absf(drift_seconds) > 0.12) or absf(drift_seconds) > seek_threshold:
		player.seek(target)
		awaiting_seek = false
		if player.has_method("set_playback_speed"): player.set_playback_speed(1.0)
	elif player.has_method("set_playback_speed"):
		player.set_playback_speed(clampf(1.0 + drift_seconds * 0.08, 0.98, 1.02) if absf(drift_seconds) > 0.04 and not desired_paused else 1.0)
	player.set_paused(desired_paused)

func finish_room_restore() -> void:
	if resume_sample.is_empty() or not loaded or not authority(): return
	var state: Dictionary = player.get_playback_state()
	if state.get("loading", false) or state.get("seeking", false): return
	var target: float = advance_room_clock(resume_sample).position
	if absf(player.get_playback_position() - target) < 0.25: resume_sample.clear()

func _process(delta: float) -> void:
	finish_room_restore()
	media_elapsed += delta
	if media_elapsed >= 0.25:
		media_elapsed = 0.0
		poll_shared_media()
	elapsed += delta
	correction_elapsed += delta
	if elapsed >= 1.0:
		elapsed = 0
		if session.is_host(): broadcast_snapshot()
		elif session.is_active(): probe_clock()
	if correction_elapsed >= 0.25:
		correction_elapsed = 0
		correct()
	var live := stream_mode()
	menu.return_to_room.visible = local_only
	menu.update_playback(player.get_playback_position(), player.get_duration(), loaded and not live)
	if live: menu.playback_time.text = "LIVE"
	menu.play_button.disabled = source.is_empty() and not local_only
	menu.play_button.text = "Reconnect live" if live else ("Play" if player.is_paused() or not loaded else "Pause")
	if live:
		menu.media_status.text = "LIVE • %s • each viewer follows the live edge" % status
	else:
		menu.media_status.text = "%s  •  %.1f / %.1f s%s" % ["Playing only here" if local_only and loaded else status, player.get_playback_position(), player.get_duration(), "  • sync %+.0f ms" % (drift_seconds * 1000) if clock_ready and not local_only else ""]

func clear_local_media() -> void:
	media_live = false
	resume_sample.clear()
	session.stop_receiving_file()
	revoke_unused_publication()
	waiting_peer_file = false
	local_path = ""
	loaded = false
	desired_paused = true
	awaiting_seek = false
	sample.clear()
	player.pause()
	player.stop()
	player.set_playback_speed(1.0)
	status = "Host has no media loaded."
	show_source("")

func show_source(path: String) -> void:
	var displayed := path if not path.is_empty() else source
	if not local_only and source_kind == "peer_file" and not source.is_empty():
		var descriptor: Variant = JSON.parse_string(source)
		displayed = descriptor.get("name", "Shared video") if descriptor is Dictionary else "Shared video"
	menu.current_source.text = displayed
	menu.current_source.tooltip_text = displayed
	var report := JSON.stringify({"source":displayed if source_kind == "peer_file" else source, "kind":source_kind, "local_path":path if source_kind == "file" else ""})
	if report != last_source_report:
		last_source_report = report
		printerr("[prim media] " + report)

func current_descriptor() -> Dictionary:
	var descriptor: Variant = JSON.parse_string(source) if source_kind == "peer_file" and not source.is_empty() else null
	return descriptor if descriptor is Dictionary else {}

func reset_local_offer() -> void:
	session.cancel_file_offer()
	pending_share_path = ""
	offered_id = ""
	offer_sent_at = 0.0

func cancel_pending_offer() -> void:
	if not pending_offer.is_empty():
		send(pending_offer.peer, {"type":"file_abort", "id":pending_offer.id})
		pending_offer.clear()

func accept_offer(peer: String, value: String) -> void:
	if not session.validate_file_offer(peer, value): return
	var descriptor: Dictionary = JSON.parse_string(value)
	if current_descriptor().get("id") == descriptor.id or pending_offer.get("id") == descriptor.id: return
	cancel_pending_offer()
	pending_offer = {"peer":peer, "source":value, "id":descriptor.id, "deadline":now() + 15.0}
	send(peer, {"type":"file_activate", "id":descriptor.id})

func revoke_unused_publication() -> void:
	var keep: String = current_descriptor().get("id", "")
	for id in published_paths.keys():
		if id != keep and id != offered_id:
			session.stop_publishing_file(id)
			published_paths.erase(id)

func open_peer_file() -> void:
	var descriptor := current_descriptor()
	waiting_peer_file = false
	loaded = false
	player.stop()
	if descriptor.get("owner") == session.get_local_id():
		local_path = published_paths.get(descriptor.get("id"), "")
		session.stop_receiving_file()
		if offered_id == descriptor.get("id"): reset_local_offer()
		if local_path.is_empty():
			send(session.get_host_id(), {"type":"file_stopped", "id":descriptor.get("id")})
			status = "Shared file unavailable."
			return
		load_local(local_path)
	else:
		waiting_peer_file = session.receive_file(source)
		status = "Connecting to shared file…" if waiting_peer_file else "Invalid shared file."
		show_source("")
	revoke_unused_publication()

func share_file(path: String) -> void:
	if not session.is_active():
		status = "Connect to friends before sharing a file."
		return
	path = PrimMenu.normalize_file_path(path)
	if path.is_empty() or not FileAccess.file_exists(path):
		status = "Choose an existing local video file."
		return
	if not offered_id.is_empty():
		send(session.get_host_id(), {"type":"file_stopped", "id":offered_id})
		if current_descriptor().get("id") != offered_id:
			session.stop_publishing_file(offered_id)
			published_paths.erase(offered_id)
	reset_local_offer()
	if session.share_file(ProjectSettings.globalize_path(path)):
		pending_share_path = ProjectSettings.globalize_path(path)
		status = "Opening shared file…"

func stop_share() -> void:
	var descriptor := current_descriptor()
	if not authority():
		if descriptor.get("owner") == session.get_local_id():
			session.stop_publishing_file(descriptor.id)
			send(session.get_host_id(), {"type":"file_stopped", "id":descriptor.id})
		return
	reset_local_offer()
	set_source("", "url")
	clear_local_media()
	broadcast_snapshot()

func peer_left(peer: String) -> void:
	if not session.is_host(): return
	if pending_offer.get("peer") == peer: cancel_pending_offer()
	if current_descriptor().get("owner") == peer:
		stop_share()
		status = "The file provider disconnected."

func poll_shared_media() -> void:
	var parsed: Variant = JSON.parse_string(session.media_state())
	var state: Dictionary = parsed if parsed is Dictionary else {}
	if not pending_offer.is_empty() and now() > pending_offer.deadline: cancel_pending_offer()
	if offer_sent_at > 0 and now() - offer_sent_at > 20:
		if not offered_id.is_empty() and current_descriptor().get("id") != offered_id:
			session.stop_publishing_file(offered_id)
			published_paths.erase(offered_id)
		reset_local_offer()
		status = "File offer timed out. Share again to retry."
	var publication: Variant = state.get("publication")
	if state.get("hosted", false) and publication is Dictionary and player.get_duration() > 0:
		state["average_mbps"] = float(publication.size) * 8.0 / player.get_duration() / 1000000.0
	state["can_stop"] = not source.is_empty() and (session.is_host() or current_descriptor().get("owner") == session.get_local_id())
	menu.update_media_share(session.is_active(), state)
	var offered: Variant = state.get("offered")
	if not pending_share_path.is_empty() and offered_id.is_empty() and offered is Dictionary:
		offered_id = offered.id
		offer_sent_at = now()
		if session.is_host():
			if session.publish_file(offered_id):
				published_paths[offered_id] = pending_share_path
				set_source(JSON.stringify(offered), "peer_file")
		else: send(session.get_host_id(), {"type":"file_offer", "source":JSON.stringify(offered)})
	var descriptor: Variant = state.get("descriptor")
	if waiting_peer_file and descriptor is Dictionary and not state.get("url", "").is_empty():
		if current_descriptor().get("id") == descriptor.get("id"):
			waiting_peer_file = false
			load_local(state.url)
	if not local_only and source_kind == "peer_file" and not state.get("status", "").is_empty(): status = state.status
	if not pending_share_path.is_empty() and not state.get("offer_status", "").is_empty(): status = state.offer_status
	if publication is Dictionary and not state.get("publication_active", true) and current_descriptor().get("id") == publication.id:
		stop_share()
		status = "Shared file changed or became unavailable."
	for viewer in state.get("viewers", {}).values():
		if viewer.get("path") == "relay" and viewer.get("consent") == "pending":
			status = "A viewer needs relay approval. Open the Sharing tab."
			break
