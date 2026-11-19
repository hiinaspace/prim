extends Node
## Godot's face of "be in a room" (round A1: a thin client, no supervision).
##
## Everything stateful lives in Rust: `prim session` (the supervisor process) starts and
## keeps the room daemon and voice and survives Godot; `PrimClient` (prim-client's room
## model) holds status, names, chat, the default-name rule and the pending-request
## table. This node (1) makes sure a session is running, (2) turns button presses into
## session requests and PrimClient calls, and (3) maps `PrimClient.room_view()` plus the
## session status onto the phases the menu draws. It never spawns or kills a process.
##
## phase: connecting -> choose (no room; see `error`) | pending (starting, or joining
##        and nobody has answered yet) | in_room | removed (the room removed me)
## Closing Godot leaves the session, the room and voice running; `quit_prim()` ends them.

signal changed

const POLL_S := 0.15
const SESSION_POLL_S := 1.0
const CONNECT_WAIT_S := 3.0

var client: PrimClient
var socket := ""
var extra_args: PackedStringArray = [] # daemon args for the next create/join: --relay, --bind, --peer-addr ...
var phase := "connecting"
var error := ""
var attached := false # true when the room was already running when the session came up
var me := ""
var lifecycle := "dead"
var room_name := ""
var members := {} # key -> {online, connected}
var names := {} # key -> display name
var invite := ""
var daemon_pid := -1 # always -1: Godot does not own the daemon (kept for older tests)
var max_members_seen := 0
var removed_members: Array = [] # keys the daemon told us were removed
var starting_kind := ""
var voice := {} # last session voice state: running, wanted, mic, deaf, state, detail, pid, starts
var session_ready := false
var _t := 0.0
var _poll := 0.0
var _session_poll := 0.0
var _pending_name := ""
var _last := ""
var _cancelling := false
var _starting := false
var _session_room := "none"
var _session_error := ""
var _calls := {} # session request id -> Callable(data)


func setup(c: PrimClient, sock: String, default_name: String) -> void:
	client = c
	socket = sock if sock != "" else c.default_socket_path()
	var env_args := OS.get_environment("PRIM_DAEMON_ARGS").strip_edges()
	if env_args != "":
		extra_args = PackedStringArray(env_args.split(" ", false))
	# The session spawns `prim` itself; point it at a dev build when there is one.
	if OS.get_environment("PRIM_BIN") == "":
		var dev := ProjectSettings.globalize_path("res://../target/debug/prim")
		if FileAccess.file_exists(dev):
			OS.set_environment("PRIM_BIN", dev)
	# The name a member gets when it has none is written by PrimClient (prim-client).
	client.set_default_name(default_name)
	client.session_reply.connect(_on_session_reply)
	client.member_event.connect(_on_member_event)
	client.removed.connect(_on_removed)
	client.connect_to(socket)
	# No session running: start one. One running: adopt it (nothing is restarted).
	_request("ensure", "", [], func(d):
		session_ready = d.get("ok", false)
		if not session_ready:
			_session_error = "Could not start the Prim session: %s" % d.get("error", "unknown error")
		_apply_session(d))


func short(key: String) -> String:
	return key.left(8)


func display_name() -> String:
	return names.get(me, "")


# ---- actions the menu calls. Each is one session request or one PrimClient call; the
# ---- same list is `prim ctl session ...` / `prim ctl ...` from a shell. -------------

func host_room(room_label := "") -> void:
	_start("create", room_label, "found")


func join_room(token: String) -> void:
	token = token.strip_edges()
	if not token.begins_with("prim1"):
		_fail("That is not an invite (it should start with prim1).")
		return
	_start("join", token, "join")


func make_invite(expiry_ms := 0) -> void:
	invite = ""
	client.make_invite(expiry_ms)


func set_display_name(n: String) -> void:
	_pending_name = n.strip_edges()
	_send_name()


func revoke(key: String) -> void:
	client.revoke(key)


func leave_room() -> void:
	_request("leave", "", [], _apply_session)


func cancel_start() -> void:
	_request("cancel", "", [], _apply_session)


func set_mic(on: bool) -> void:
	_request("mic_on" if on else "mic_off", "", [], _apply_session)


func set_deafen(on: bool) -> void:
	_request("deafen_on" if on else "deafen_off", "", [], _apply_session)


func set_voice(on: bool) -> void:
	_request("voice_on" if on else "voice_off", "", [], _apply_session)


func send_chat(text: String) -> void:
	client.chat_send(text)


## Close Godot and leave the session, room and voice running (what the window's X does).
func close_window() -> void:
	get_tree().quit()


## Stop everything (voice, the room daemon, the session), then close Godot.
func quit_prim() -> void:
	_request("quit", "", [], func(_d): get_tree().quit())


## Stop the session without closing Godot (tests clean up with this).
func quit_session(done := Callable()) -> void:
	_request("quit", "", [], func(d):
		if done.is_valid():
			done.call(d))


func _notification(what: int) -> void:
	if what == NOTIFICATION_WM_CLOSE_REQUEST:
		# Closing the window is not leaving: the session keeps the room and voice.
		close_window()


# ---- internals -------------------------------------------------------------------

func _start(op: String, arg: String, kind: String) -> void:
	error = ""
	_cancelling = false
	_starting = true
	starting_kind = kind
	phase = "pending"
	_emit()
	_request(op, arg, extra_args, func(d):
		_starting = false
		if not d.get("ok", false):
			_fail(str(d.get("error", "Could not start the room.")))
		_apply_session(d))


func _request(op: String, arg: String, extra, cb: Callable) -> void:
	var id := client.session_request(op, arg, PackedStringArray(extra))
	_calls[id] = cb


func _on_session_reply(id: int, data: Dictionary) -> void:
	var cb: Callable = _calls.get(id, Callable())
	_calls.erase(id)
	if cb.is_valid():
		cb.call(data)


func _apply_session(d: Dictionary) -> void:
	if not d.get("ok", false):
		return
	voice = d.get("voice", {})
	_session_room = d.get("room", "none")
	attached = d.get("kind", "") == "attached"
	if _session_room == "failed" and error == "":
		error = str(d.get("error", ""))
	_emit()


func _fail(msg: String) -> void:
	error = msg
	if phase == "pending":
		phase = "choose"
	_emit()


func _emit() -> void:
	changed.emit()


func _send_name() -> void:
	if _pending_name != "" and me != "" and lifecycle == "open":
		client.set_display_name(_pending_name)
		_pending_name = ""


func _process(dt: float) -> void:
	_t += dt
	if client == null:
		return
	_poll -= dt
	if _poll <= 0.0:
		_poll = POLL_S
		_refresh()
	_session_poll -= dt
	if _session_poll <= 0.0:
		_session_poll = SESSION_POLL_S
		if session_ready:
			_request("status", "", [], _apply_session)


func _s(v: Dictionary, key: String, fallback := ""):
	var x = v.get(key, null)
	return fallback if x == null else x


func _refresh() -> void:
	var v: Dictionary = client.room_view()
	me = _s(v, "me")
	lifecycle = _s(v, "lifecycle", "dead")
	room_name = _s(v, "room_name")
	var seen := {}
	var nm := {}
	for m in v.get("members", []):
		seen[m["key"]] = {"online": m.get("presence", "") == "online", "connected": true}
		var n: String = m.get("name", "")
		nm[m["key"]] = "" if n == m["key"].left(8) else n
	members = seen
	names = nm
	max_members_seen = maxi(max_members_seen, seen.size())
	var inv: String = _s(v, "invite")
	if inv != "":
		invite = inv
	var daemon_up: bool = v.get("attached", false)
	if v.get("removed", false):
		phase = "removed"
		error = "You were removed from the room."
	elif v.get("in_room", false) and daemon_up:
		phase = "in_room"
		_send_name()
	elif _s(v, "join", "none") == "refused":
		if error == "":
			error = _refusal_text(str(_s(v, "join_refused")))
		phase = "choose"
		if not _cancelling:
			_cancelling = true
			cancel_start() # stop the daemon so the person can try another invite
	elif daemon_up or _starting or _session_room == "starting":
		phase = "pending"
	elif not session_ready and _t < CONNECT_WAIT_S and phase == "connecting":
		pass
	else:
		phase = "choose"
		if _session_error != "" and error == "":
			error = _session_error
	var sig := JSON.stringify([phase, error, me, lifecycle, room_name, members, names, invite, voice])
	if sig != _last:
		_last = sig
		_emit()


func _refusal_text(why: String) -> String:
	match why:
		"invite_invalid", "not_member":
			return "Not admitted: that invite was already used, expired or revoked."
		"closed":
			return "That room is closed."
		"revoked":
			return "You were removed from that room."
		"wrong_room", "version", "unsupported":
			return "Could not join: that room runs an incompatible version."
	return "Not admitted (%s)." % why


func _on_member_event(key: String, what: String) -> void:
	if what == "removed":
		removed_members.append(key)
	_emit()


func _on_removed() -> void:
	phase = "removed"
	error = "You were removed from the room."
	_emit()
