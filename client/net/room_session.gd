extends Node
## C1: the Godot side of "be in a room". Everything goes through the room daemon over
## room.sock (PrimClient): attach to a room that is already running (for example one
## `prim ui` started), or start a daemon here to host or join, mint an invite, show who
## I am, and write my display name as `member/<me>/profile` ({"v":1,"name":...}, durable).
##
## phase: connecting -> choose (no daemon, or ours exited: see `error`) | pending (daemon up,
##        not in a room yet) | in_room | removed (the room removed me)
## Nothing here knows about 3D; the menu (ui/room_menu.gd) and the tests drive it.

signal changed

const PROFILE_VERSION := 1
const ATTACH_WAIT_S := 2.0
const JOIN_TIMEOUT_S := 45.0

var client: PrimClient
var socket := ""
var prim_bin := ""
var extra_args: PackedStringArray = [] # daemon args: --relay, --bind, --peer-addr ...
var phase := "connecting"
var error := ""
var attached := false # true when the daemon was already running (not started by us)
var me := ""
var lifecycle := "dead"
var members := {} # key -> {online, connected}
var names := {} # key -> display name (from member/*/profile)
var invite := ""
var daemon_pid := -1
var daemon_log := ""
var max_members_seen := 0
var removed_members: Array = [] # keys the daemon told us were removed
var _sub_profile := -1
var _profile_snapshot := false
var _wanted_name := ""
var _written_name := "\u0000"
var _invite_req := -1
var _t := 0.0
var _poll := 0.0
var _started_at := 0.0
var _starting_kind := ""
var _explicit_name := false


func setup(c: PrimClient, sock: String, default_name: String) -> void:
	client = c
	socket = sock if sock != "" else c.default_socket_path()
	_wanted_name = default_name
	prim_bin = _find_prim()
	var env_args := OS.get_environment("PRIM_DAEMON_ARGS").strip_edges()
	if env_args != "":
		extra_args = PackedStringArray(env_args.split(" ", false))
	client.connected.connect(_on_connected)
	client.disconnected.connect(_emit)
	client.reply.connect(_on_reply)
	client.state_value.connect(_on_state_value)
	client.snapshot_done.connect(_on_snapshot_done)
	client.state_reset.connect(_on_state_reset)
	client.member_event.connect(_on_member_event)
	client.removed.connect(_on_removed)
	client.connect_to(socket)
	_sub_profile = client.subscribe("member/*/profile")


func _find_prim() -> String:
	var e := OS.get_environment("PRIM_BIN")
	if e != "":
		return e
	var dev := ProjectSettings.globalize_path("res://../target/debug/prim")
	if FileAccess.file_exists(dev):
		return dev
	return "prim"


func _emit() -> void:
	changed.emit()


func display_name() -> String:
	return names.get(me, "")


func short(key: String) -> String:
	return key.left(8)


## Actions the menu calls ----------------------------------------------------------

func host_room() -> void:
	_spawn(["room", "found", "--meta", "godot"], "host")


func join_room(token: String) -> void:
	token = token.strip_edges()
	if not token.begins_with("prim1"):
		_fail("That is not an invite (it should start with prim1).")
		return
	_spawn(["room", "join", token], "join")


func make_invite(expiry_ms := 3600000) -> void:
	invite = ""
	_invite_req = client.create_invite(expiry_ms)
	if _invite_req < 0:
		_fail("Not connected to the room daemon.")


func set_display_name(n: String) -> void:
	_explicit_name = true
	_wanted_name = n.strip_edges()
	_write_profile()


func revoke(key: String) -> void:
	client.revoke(key)


func leave_room() -> void:
	client.leave()


func cancel_start() -> void:
	if daemon_pid > 0:
		OS.kill(daemon_pid)
		daemon_pid = -1
	phase = "choose"
	error = ""
	_emit()


func stop_daemon() -> void:
	## Only the process this client started.
	if daemon_pid > 0 and OS.is_process_running(daemon_pid):
		OS.kill(daemon_pid)
	daemon_pid = -1


# ---- internals -------------------------------------------------------------------

func _spawn(args: Array, kind: String) -> void:
	if phase in ["pending", "in_room"] and daemon_pid > 0:
		return
	error = ""
	var sock_dir := socket.get_base_dir()
	DirAccess.make_dir_recursive_absolute(sock_dir)
	daemon_log = sock_dir.path_join("godot-daemon-%d.log" % Time.get_ticks_msec())
	var full: Array = [prim_bin] + args
	full += ["--socket", socket, "--rt-socket", sock_dir.path_join("rt.sock")]
	full += Array(extra_args)
	# sh -c 'exec "$@" >log 2>&1' LOG prim ...: the daemon owns its output file.
	var sh_args: Array = ["-c", 'log="$1"; shift; exec "$@" >"$log" 2>&1', "sh", daemon_log] + full
	daemon_pid = OS.create_process("/bin/sh", PackedStringArray(sh_args))
	if daemon_pid <= 0:
		_fail("Could not start the room daemon (%s)." % prim_bin)
		return
	phase = "pending"
	_starting_kind = kind
	_started_at = _t
	attached = false
	_emit()


func _fail(msg: String) -> void:
	error = msg
	if phase == "pending":
		phase = "choose"
	_emit()


func _log_tail() -> String:
	if daemon_log == "" or not FileAccess.file_exists(daemon_log):
		return ""
	var lines := FileAccess.get_file_as_string(daemon_log).strip_edges().split("\n")
	return lines[lines.size() - 1] if lines.size() > 0 else ""


func _process(dt: float) -> void:
	_t += dt
	if client == null:
		return
	if phase == "connecting" and _t > ATTACH_WAIT_S and not client.is_daemon_connected():
		phase = "choose"
		_emit()
	_poll -= dt
	if _poll <= 0.0:
		_poll = 0.5
		if client.is_daemon_connected():
			client.request_status()
	if phase == "pending" and daemon_pid > 0:
		if not OS.is_process_running(daemon_pid) and not client.is_daemon_connected():
			var tail := _log_tail()
			daemon_pid = -1
			phase = "choose"
			error = ("The room could not be %s: %s" % ["joined" if _starting_kind == "join" else "started", tail]) \
				if tail != "" else "The room daemon exited."
			_emit()
		elif _t - _started_at > JOIN_TIMEOUT_S and lifecycle != "open":
			stop_daemon()
			phase = "choose"
			error = "Not admitted: nobody in that room accepted this invite (it may be used up, expired, revoked or the host is offline)."
			_emit()


func _on_connected() -> void:
	if phase == "connecting" or phase == "choose":
		attached = daemon_pid <= 0
		phase = "pending"
	_emit()


func _on_reply(req: int, data: Dictionary) -> void:
	var kind: String = data.get("kind", "")
	if kind == "status":
		_apply_status(data["status"])
	elif req == _invite_req:
		_invite_req = -1
		if kind == "invite":
			invite = data.get("token", "")
		else:
			error = "Could not make an invite: %s" % data.get("code", kind)
		_emit()


func _apply_status(st: Dictionary) -> void:
	me = st.get("me", "")
	lifecycle = st.get("lifecycle", "dead")
	var seen := {}
	for m in st.get("members", []):
		seen[m["key"]] = {"online": m.get("online", false), "connected": m.get("connected", false)}
	members = seen
	max_members_seen = maxi(max_members_seen, seen.size())
	if lifecycle in ["open", "closed"] and phase in ["pending", "connecting", "choose"]:
		phase = "in_room"
	_write_profile()
	_emit()


func _on_state_value(sub: int, path: String, _writer: String, _live: bool, value: PackedByteArray) -> void:
	if sub != _sub_profile:
		return
	var parts := path.split("/")
	if parts.size() != 3 or parts[2] != "profile":
		return
	var parsed = JSON.parse_string(value.get_string_from_utf8())
	if parsed is Dictionary:
		names[parts[1]] = str(parsed.get("name", ""))
		_emit()


func _on_snapshot_done(sub: int) -> void:
	if sub == _sub_profile:
		_profile_snapshot = true
		_write_profile()


func _on_state_reset(sub: int) -> void:
	if sub == _sub_profile:
		names.clear()
		_profile_snapshot = false
		_written_name = "\u0000"


func _on_member_event(key: String, what: String) -> void:
	if what == "removed":
		removed_members.append(key)
	_emit()


func _on_removed() -> void:
	phase = "removed"
	error = "You were removed from the room."
	_emit()


## Write the profile once the snapshot is in, so a name already on the room (a rejoin,
## or `prim ui`'s) isn't overwritten by our default. An explicit set_name always writes.
func _write_profile() -> void:
	if me == "" or lifecycle != "open" or not _profile_snapshot or _wanted_name == "":
		return
	if _wanted_name == _written_name:
		return
	if _written_name == "\u0000" and names.get(me, "") != "" and not _explicit_name:
		_written_name = names[me]
		return
	_written_name = _wanted_name
	var body := JSON.stringify({"v": PROFILE_VERSION, "name": _wanted_name})
	client.write_state("member", "profile", false, true, body.to_utf8_buffer())
