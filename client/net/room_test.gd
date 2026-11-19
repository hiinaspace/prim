extends Node
## C1 oracle roles for tools/test-room-join.py, one Godot process per role, each with its
## own daemon, started and driven through the same buttons a person uses.
##   host     hosts, names itself, mints invites, sees the guest, then revokes it
##   guest    pastes the host's invite, must be in the room and see the host's name
##   forged   a token with one character changed: must be refused
##   replay   the guest's already-used token: must be refused
##   expired  a 1.5 s invite used after it expired: must be refused
## Files in --test-dir carry the handshake (invite, host key, port) between roles.
## ROOM_TEST_FAULT: "no-revoke" (host never revokes), "valid-token" (the forged role gets
## the real token) -- the negative controls that must turn the report red.

var app
var session
var menu
var dir := ""
var role := ""
var checks: Array = []
var fault := OS.get_environment("ROOM_TEST_FAULT")


func run(main, r: String, d: String) -> void:
	app = main
	session = main.session
	menu = main.menu
	role = r
	dir = d
	_go.call_deferred()


func _check(name: String, ok: bool, detail := {}) -> bool:
	checks.append({"name": name, "pass": ok, "detail": detail})
	print("room-test[%s]: %s %s %s" % [role, "PASS" if ok else "FAIL", name, JSON.stringify(detail)])
	return ok


func _wait(pred: Callable, secs: float) -> bool:
	var end := Time.get_ticks_msec() + int(secs * 1000.0)
	while not pred.call() and Time.get_ticks_msec() < end:
		await get_tree().process_frame
	return pred.call()


func _file(name: String) -> String:
	return dir.path_join(name)


func _put(name: String, v: Dictionary) -> void:
	var f := FileAccess.open(_file(name + ".tmp"), FileAccess.WRITE)
	f.store_string(JSON.stringify(v))
	f.close()
	DirAccess.rename_absolute(_file(name + ".tmp"), _file(name))


func _get(name: String, secs: float) -> Dictionary:
	if not await _wait(func(): return FileAccess.file_exists(_file(name)), secs):
		return {}
	await get_tree().create_timer(0.2).timeout
	var p = JSON.parse_string(FileAccess.get_file_as_string(_file(name)))
	return p if p is Dictionary else {}


func _go() -> void:
	match role:
		"host": await _host()
		"guest": await _guest()
		"forged", "replay", "expired": await _intruder()
	_finish()


func _set_name_and_wait(n: String) -> void:
	menu.name_edit.text = n
	menu.name_button.pressed.emit()
	await _wait(func(): return session.display_name() == n, 10.0)


func _host() -> void:
	await _wait(func(): return session.phase == "choose", 10.0)
	_check("no baked secret: starts at the host/join screen", session.phase == "choose" and session.me == "")
	menu.name_edit.text = "Hosty"
	menu.host_button.pressed.emit()
	var ok: bool = await _wait(func(): return session.phase == "in_room" and session.me != "", 30.0)
	_check("host: founded a room through the daemon", ok, {"me": session.short(session.me), "lifecycle": session.lifecycle})
	await _set_name_and_wait("Hosty")
	_check("host: profile round-trips as member/<id>/profile", session.display_name() == "Hosty")
	menu.invite_button.pressed.emit()
	await _wait(func(): return menu.invite_edit.text.begins_with("prim1"), 10.0)
	var tok: String = menu.invite_edit.text
	_check("host: invite minted and shown", tok.begins_with("prim1"), {"len": tok.length()})
	# A short-lived invite: the 'expired' role uses it after it has lapsed.
	var short_tok := ""
	session._invite_req = session.client.create_invite(1500)
	await _wait(func(): return session.invite != tok and session.invite != "", 10.0)
	short_tok = session.invite
	_put("host", {"key": session.me, "token": tok, "short_token": short_tok,
		"port": int(OS.get_environment("ROOM_TEST_PORT"))})
	var g: Dictionary = await _get("guest.json", 60.0)
	var gk: String = g.get("key", "")
	ok = gk != "" and await _wait(func(): return session.members.has(gk), 30.0)
	_check("host: roster shows the guest", ok)
	await _wait(func(): return session.names.get(gk, "") == "Guesty", 10.0)
	_check("host: sees the guest's name", session.names.get(gk, "") == "Guesty", {"name": session.names.get(gk, "")})
	for r in ["forged", "replay", "expired"]:
		var d: Dictionary = await _get(r + ".done", 90.0)
		_check("host: %s intruder finished" % r, not d.is_empty())
	_check("host: nobody else ever joined", session.max_members_seen == 2 and session.members.size() == 2,
		{"max_seen": session.max_members_seen, "now": session.members.size()})
	if fault != "no-revoke":
		session.revoke(gk)
		var gone: bool = await _wait(func(): return not session.members.has(gk) or session.removed_members.has(gk), 20.0)
		_check("host: revoked guest leaves the roster", gone, {"removed": session.removed_members.size()})
	_put("host.done", {})
	await get_tree().create_timer(2.0).timeout


func _guest() -> void:
	var h: Dictionary = await _get("host.json", 60.0)
	if not _check("guest: got the host's invite file", not h.is_empty()):
		return
	session.extra_args.append_array(["--peer-addr", "%s=127.0.0.1:%d" % [h["key"], h["port"]]])
	menu.name_edit.text = "Guesty"
	menu.token_edit.text = h["token"]
	menu.join_button.pressed.emit()
	var ok: bool = await _wait(func(): return session.phase == "in_room" and session.members.size() >= 2, 40.0)
	_check("guest: joined by invite code", ok, {"phase": session.phase, "members": session.members.size(), "err": session.error})
	await _set_name_and_wait("Guesty")
	_check("guest: sees the host in the roster by name", session.names.get(h["key"], "") == "Hosty",
		{"name": session.names.get(h["key"], "")})
	_check("guest: is not the host", session.me != h["key"] and session.me != "")
	_put("guest", {"key": session.me})
	var removed: bool = await _wait(func(): return session.phase == "removed", 90.0 if fault != "no-revoke" else 25.0)
	_check("guest: removed once the host revokes it", removed, {"phase": session.phase})
	_put("guest.done", {})


func _intruder() -> void:
	var h: Dictionary = await _get("host.json", 60.0)
	if not _check("%s: got the host's invite file" % role, not h.is_empty()):
		_put(role + ".done", {})
		return
	var tok: String = h["token"]
	match role:
		"forged":
			if fault != "valid-token":
				var i := tok.length() / 2
				tok = tok.left(i) + ("a" if tok[i] != "a" else "b") + tok.substr(i + 1)
		"replay":
			await _get("guest.json", 60.0) # only after the guest has spent it
			await get_tree().create_timer(1.0).timeout
		"expired":
			tok = h["short_token"]
			await get_tree().create_timer(4.0).timeout
	session.extra_args.append_array(["--peer-addr", "%s=127.0.0.1:%d" % [h["key"], h["port"]]])
	menu.token_edit.text = tok
	menu.join_button.pressed.emit()
	var got_in: bool = await _wait(func(): return session.phase == "in_room", 30.0)
	_check("%s: refused (never in the room)" % role, not got_in,
		{"phase": session.phase, "members": session.members.size(), "error": session.error})
	_check("%s: says why" % role, session.error != "" or not got_in, {"error": session.error})
	_put(role + ".done", {"error": session.error})


func _finish() -> void:
	var failures := checks.filter(func(c): return not c["pass"]).size()
	var f := FileAccess.open(_file(role + ".report.json"), FileAccess.WRITE)
	f.store_string(JSON.stringify({"role": role, "checks": checks, "failures": failures,
		"me": session.me, "phase": session.phase}, "  "))
	f.close()
	session.stop_daemon()
	get_tree().quit(1 if failures else 0)
