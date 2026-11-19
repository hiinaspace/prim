extends Node3D
## Loop room shell: owns the daemon connection (PrimClient), the shared room model, hot
## reload of res://room/*, VR/desktop view setup, and the --verify oracles.
##
## User args (after `--`): --socket PATH  --desktop  --verify  --hold SECS
##   --name DISPLAY_NAME  (C1) --role host|guest|forged|replay|expired --test-dir DIR
##   --report FILE  --screenshot FILE
## Env: PRIM_SOCKET, PRIM_EVENT_LOG (JSONL event sink), PRIM_FAULT (views1: attach the
##   eye probe to a mono viewport, which must trip "render views == scene views").

const ROOM_DIR := "res://room"
const VIEW_SCRIPT := "res://room/room_view.gd"

## What the daemon says, kept across hot reloads.
class RoomModel:
	extends RefCounted
	var me := ""
	var lifecycle := "dead"
	var coordinator := ""
	var members := {} # key -> {online, connected}
	var names := {} # key -> display name
	var poses := {} # key -> {pos: Vector3, yaw: float, n: int, recv_ms: int}
	var pose_log := {} # key -> Array of [recv_ms, Vector3]
	var connected := false
	var reloads := 0

	func short(key: String) -> String:
		return key.left(6)

	func roster_text() -> String:
		var lines := ["PRIM loop  |  daemon %s  |  room %s" % [
			"up" if connected else "DOWN", lifecycle]]
		lines.append("members: %d" % members.size())
		for key in members:
			var label: String = names.get(key, "")
			var tag := " (you)" if key == me else ""
			var m: Dictionary = members[key]
			lines.append("  %s %s%s  %s" % [short(key), label, tag,
				"online" if m.get("online", false) else "offline"])
		return "\n".join(lines)

	func remote_keys() -> Array:
		return members.keys().filter(func(k): return k != me)


var args := {}
var want_xr := true
var client: PrimClient
var model := RoomModel.new()
var view: Node3D
var camera: Camera3D
var xr_camera: XRCamera3D
var xr_active := false
var xrprobe: XRProbe
var probe_vp: SubViewport
var session # RoomSession (net/room_session.gd): host/join/invite/identity/profile
var menu # RoomMenu (ui/room_menu.gd)
var sub_pose := -1
var sub_profile := -1
var _status_timer := 0.0
var _reload_timer := 0.0
var _mtimes := {}
var _first_frame_sent := false
var _joined_sent := false
var _peer_pose_sent := false
var _checks: Array = []
var _verify_started := false


func _ready() -> void:
	_parse_args()
	PrimEvents.emit("spawn", JSON.stringify({"what": "godot", "xr": want_xr}))
	_setup_camera()
	_load_view()
	client = PrimClient.new()
	client.name = "PrimClient"
	add_child(client)
	client.connected.connect(_on_connected)
	client.disconnected.connect(func(): model.connected = false)
	client.reply.connect(_on_reply)
	client.state_value.connect(_on_state_value)
	client.state_removed.connect(_on_state_removed)
	var sock: String = args.get("socket", OS.get_environment("PRIM_SOCKET"))
	print("prim-loop: socket=%s xr=%s verify=%s" % [sock, xr_active, args.has("verify")])
	session = preload("res://net/room_session.gd").new()
	session.name = "RoomSession"
	add_child(session)
	session.setup(client, sock, str(args.get("name", OS.get_environment("PRIM_NAME"))))
	menu = preload("res://ui/room_menu.gd").new()
	menu.name = "RoomMenu"
	add_child(menu)
	menu.setup(session)
	if args.has("role"):
		var t = preload("res://net/room_test.gd").new()
		t.name = "RoomTest"
		add_child(t)
		t.run(self, args["role"], args.get("test-dir", ""))
	sub_pose = client.subscribe("member/*/pose")
	sub_profile = client.subscribe("member/*/profile")
	RenderingServer.frame_post_draw.connect(_on_first_frame, CONNECT_ONE_SHOT)
	if args.has("verify"):
		_verify.call_deferred()


func _parse_args() -> void:
	var a := OS.get_cmdline_user_args()
	var i := 0
	while i < a.size():
		var k: String = a[i]
		if k in ["--socket", "--hold", "--report", "--screenshot", "--name", "--role", "--test-dir"]:
			args[k.substr(2)] = a[i + 1]
			i += 2
		else:
			args[k.substr(2)] = true
			i += 1
	want_xr = not args.has("desktop")


func _setup_camera() -> void:
	if want_xr:
		var xr := XRServer.find_interface("OpenXR")
		if xr != null and xr.is_initialized():
			get_viewport().use_xr = true
			xr_active = true
	var origin := XROrigin3D.new()
	origin.name = "XROrigin3D"
	add_child(origin)
	if xr_active:
		xr_camera = XRCamera3D.new()
		xr_camera.position = Vector3(0, 1.6, 0)
		origin.add_child(xr_camera)
		camera = xr_camera
		for hand in ["left_hand", "right_hand"]:
			var c := XRController3D.new()
			c.tracker = hand
			origin.add_child(c)
	else:
		camera = Camera3D.new()
		camera.position = Vector3(0, 1.7, 3.5)
		add_child(camera)
		camera.look_at(Vector3(0, 1.1, -1.0))
		camera.current = true


func _load_view() -> void:
	if view:
		view.queue_free()
	# CACHE_MODE_REPLACE: re-read edited scripts and scenes from disk.
	var script: GDScript = ResourceLoader.load(VIEW_SCRIPT, "", ResourceLoader.CACHE_MODE_REPLACE)
	view = Node3D.new()
	view.name = "RoomView"
	view.set_script(script)
	add_child(view)
	view.setup(model, not xr_active)
	_snapshot_mtimes()


func _snapshot_mtimes() -> bool:
	var changed := false
	for f in DirAccess.get_files_at(ROOM_DIR):
		var t := FileAccess.get_modified_time(ROOM_DIR.path_join(f))
		if _mtimes.get(f, t) != t:
			changed = true
		_mtimes[f] = t
	return changed


func _process(dt: float) -> void:
	_status_timer -= dt
	if _status_timer <= 0.0:
		_status_timer = 0.5
		if client.is_daemon_connected():
			client.request_status()
	_reload_timer -= dt
	if _reload_timer <= 0.0:
		_reload_timer = 0.5
		if _snapshot_mtimes():
			model.reloads += 1
			print("prim-loop: reloading room view (#%d)" % model.reloads)
			_load_view()
			PrimEvents.emit("reloaded", JSON.stringify({"n": model.reloads}))


func _on_first_frame() -> void:
	if not _first_frame_sent:
		_first_frame_sent = true
		PrimEvents.emit("first_frame", "{}")


func _on_connected() -> void:
	model.connected = true
	PrimEvents.emit("daemon_ready", "{}")


func _on_reply(_req: int, data: Dictionary) -> void:
	if data.get("kind", "") != "status":
		return
	var st: Dictionary = data["status"]
	model.me = st.get("me", "")
	model.lifecycle = st.get("lifecycle", "dead")
	var coord = st.get("coordinator")
	model.coordinator = coord.get("holder", "") if coord is Dictionary else ""
	var seen := {}
	for m in st.get("members", []):
		seen[m["key"]] = {"online": m.get("online", false), "connected": m.get("connected", false)}
	model.members = seen
	if not _joined_sent and model.lifecycle == "open" and seen.size() >= 2:
		_joined_sent = true
		PrimEvents.emit("joined", JSON.stringify({"members": seen.size()}))


func _on_state_value(_sub: int, path: String, writer: String, _live: bool, value: PackedByteArray) -> void:
	var parts := path.split("/")
	if parts.size() != 3 or parts[0] != "member":
		return
	var parsed = JSON.parse_string(value.get_string_from_utf8())
	if not (parsed is Dictionary):
		return
	if parts[2] == "profile":
		model.names[writer] = str(parsed.get("name", ""))
	elif parts[2] == "pose" and parsed.has("pos"):
		var p: Array = parsed["pos"]
		var pos := Vector3(p[0], p[1], p[2])
		var now := Time.get_ticks_msec()
		model.poses[writer] = {"pos": pos, "yaw": float(parsed.get("yaw", 0.0)),
			"n": int(parsed.get("n", 0)), "recv_ms": now}
		var log: Array = model.pose_log.get(writer, [])
		log.append([now, pos])
		if log.size() > 600:
			log = log.slice(log.size() - 600)
		model.pose_log[writer] = log
		if writer != model.me and not _peer_pose_sent:
			_peer_pose_sent = true
			PrimEvents.emit("peer_pose", JSON.stringify({"writer": writer}))


func _on_state_removed(_sub: int, path: String) -> void:
	var parts := path.split("/")
	if parts.size() == 3 and parts[2] == "pose":
		model.poses.erase(parts[1])


# ---- verification ------------------------------------------------------------------

func _check(name: String, ok: bool, detail := {}) -> bool:
	_checks.append({"name": name, "pass": ok, "detail": detail})
	print("prim-loop: %s %s %s" % ["PASS" if ok else "FAIL", name, JSON.stringify(detail)])
	if xrprobe:
		xrprobe.check(name, ok, detail)
	return ok


func _wait(pred: Callable, secs: float) -> bool:
	var end := Time.get_ticks_msec() + int(secs * 1000.0)
	while not pred.call() and Time.get_ticks_msec() < end:
		await get_tree().process_frame
	return pred.call()


## Largest distance between any two samples of `key`'s pose in the last `window_s`.
func _travel(key: String, window_s: float) -> float:
	var now := Time.get_ticks_msec()
	var pts: Array = []
	for s in model.pose_log.get(key, []):
		if now - s[0] <= int(window_s * 1000.0):
			pts.append(s[1])
	var best := 0.0
	for i in pts.size():
		for j in range(i + 1, pts.size()):
			best = maxf(best, (pts[i] as Vector3).distance_to(pts[j]))
	return best


func _verify() -> void:
	if _verify_started:
		return
	_verify_started = true
	var fault := OS.get_environment("PRIM_FAULT")
	if OS.get_environment("XRPROBE_OUT") != "" or xr_active:
		xrprobe = XRProbe.new()
		add_child(xrprobe)
	if xr_active and xrprobe:
		var probe_cam: Camera3D = xr_camera
		if fault == "views1":
			# A mono viewport rendering while the XR server reports two views: the probe
			# then sees 1 render view against 2 scene views.
			probe_vp = SubViewport.new()
			probe_vp.size = Vector2i(256, 256)
			probe_vp.render_target_update_mode = SubViewport.UPDATE_ALWAYS
			var c := Camera3D.new()
			c.position = Vector3(0, 1.6, 3)
			probe_vp.add_child(c)
			c.current = true
			add_child(probe_vp)
			probe_cam = c
		xrprobe.attach(probe_cam)
		await xrprobe.boot_check()
	var hold := float(args.get("hold", "8"))
	# 1. The room: two members, and the bot's pose arriving.
	var roster_ok: bool = await _wait(func(): return model.members.size() >= 2 and model.lifecycle == "open", 40.0)
	_check("roster shows 2 members", roster_ok and model.members.size() == 2,
		{"members": model.members.keys().map(func(k): return model.short(k)), "lifecycle": model.lifecycle})
	var remote: Array = model.remote_keys()
	var pose_ok: bool = await _wait(func(): return not model.remote_keys().is_empty() and model.poses.has(model.remote_keys()[0]), 20.0)
	_check("bot pose received", pose_ok)
	var t0 := Time.get_ticks_msec()
	# 2. Per-eye captures (VR) while the bot moves.
	if xr_active and xrprobe:
		for _i in 20:
			await get_tree().process_frame
		var rec: Dictionary = await xrprobe.capture("room")
		if rec.is_empty():
			_check("per-eye capture", false, {"reason": "no capture"})
		else:
			var eyes_ok: bool = xrprobe.check_capture(rec, {"sky": true, "world": true})
			_checks.append({"name": "per-eye capture (views, non-black, sky/world)", "pass": eyes_ok,
				"detail": {"buffers": rec.get("buffers"), "scene": rec.get("scene")}})
			var bp: Variant = view.marker_position(remote[0]) if not remote.is_empty() else null
			_check("bot in view of the camera at capture",
				bp != null and camera.is_position_in_frustum((bp as Vector3) + Vector3(0, 0.8, 0)),
				{"marker": str(bp)})
	# 3. Hold, then judge movement over the final window only (a bot that died mid-run
	#    must fail even though it moved earlier).
	var remaining := hold - (Time.get_ticks_msec() - t0) / 1000.0
	if remaining > 0.0:
		await get_tree().create_timer(remaining).timeout
	if not remote.is_empty():
		var key: String = remote[0]
		var travel := _travel(key, 3.0)
		var age_ms := Time.get_ticks_msec() - int(model.poses.get(key, {}).get("recv_ms", 0))
		_check("bot marker moves (last 3 s)", travel > 0.3 and age_ms < 1500,
			{"travel_m": snappedf(travel, 0.01), "last_pose_age_ms": age_ms})
		var vp: Variant = view.marker_position(key)
		var want: Vector3 = model.poses.get(key, {}).get("pos", Vector3.ZERO)
		_check("marker follows pose", vp != null and (vp as Vector3).distance_to(want) < 0.1,
			{"marker": str(vp), "pose": str(want)})
	else:
		_check("bot marker moves (last 3 s)", false, {"reason": "no remote member"})
	if args.has("screenshot"):
		await RenderingServer.frame_post_draw
		get_viewport().get_texture().get_image().save_png(args["screenshot"])
	_finish()


func _finish() -> void:
	var failures := _checks.filter(func(c): return not c["pass"]).size()
	if args.has("report"):
		var f := FileAccess.open(args["report"], FileAccess.WRITE)
		f.store_string(JSON.stringify({"checks": _checks, "failures": failures,
			"xr": xr_active, "fault": OS.get_environment("PRIM_FAULT"),
			"members": model.members.size(), "reloads": model.reloads}, "  "))
		f.close()
	if xrprobe:
		xrprobe.event("end", {"failures": xrprobe.failures + 0})
	print("prim-loop: verify done, failures=%d" % failures)
	get_tree().quit(1 if failures else 0)
