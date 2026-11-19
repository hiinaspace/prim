extends Node
## C2 oracle, driven by tools/test-pose.py against real daemons and prim-bot.
##   --pose-test main   avatar appears and tracks the bot's known pose within tolerance, keeps
##                      moving smoothly, left/right hands on the correct sides (own frame and
##                      as seen by the viewer), name label; also publishes this client's own head
##                      from a camera that is moved mid-run (the bot's --watch log is checked)
##   --pose-test stop   the bot stops publishing (stays attached): the avatar must vanish
##   --pose-test kill   the bot's daemon is killed by the harness: the avatar must vanish
## Writes <test-dir>/<mode>.report.json. PRIM_FAULT: nointerp | mirror-x | stale make the
## matching check go red.

const HZ := 20.0
var app
var dir := ""
var mode := ""
var checks: Array = []
var bot_key := ""


func run(main, m: String, d: String) -> void:
	app = main
	mode = m
	dir = d
	_go.call_deferred()


func _check(name: String, ok: bool, detail := {}) -> bool:
	checks.append({"name": name, "pass": ok, "detail": detail})
	print("pose-test[%s]: %s %s %s" % [mode, "PASS" if ok else "FAIL", name, JSON.stringify(detail)])
	return ok


func _wait(pred: Callable, secs: float) -> bool:
	var end := Time.get_ticks_msec() + int(secs * 1000.0)
	while not pred.call() and Time.get_ticks_msec() < end:
		await get_tree().process_frame
	return pred.call()


func _drop(name: String, v: Dictionary) -> void:
	var f := FileAccess.open(dir.path_join(name + ".tmp"), FileAccess.WRITE)
	f.store_string(JSON.stringify(v))
	f.close()
	DirAccess.rename_absolute(dir.path_join(name + ".tmp"), dir.path_join(name))


## The bot's pose at time t (prim-bot.rs `bot_pose`): written down again here so the check
## is not the code it checks.
static func bot_pose(t: float) -> Dictionary:
	var a := 0.6 * t
	var head := Vector3(0.9 * sin(a), 1.65, -1.4 + 0.2 * cos(a))
	var yaw := PI + 0.5 * sin(a)
	var b := Basis(Vector3.UP, yaw)
	return {
		"head": head,
		"left": head + b * Vector3(-0.30, 0.25 + 0.05 * sin(3.0 * a), -0.30),
		"right": head + b * Vector3(0.30, -0.55, -0.15),
		"yaw": yaw,
	}


func _go() -> void:
	var ok: bool = await _wait(func(): return app.session.lifecycle == "open" and app.avatars.visible_keys().size() > 0, 40.0)
	if not _check("an avatar appears for the other member", ok):
		_finish()
		return
	bot_key = app.avatars.visible_keys()[0]
	# Exactly one avatar, and it is not our own.
	_check("exactly one avatar is drawn and it is not ours",
		app.avatars.visible_keys().size() == 1 and bot_key != app.session.me, {"keys": app.avatars.visible_keys(), "me": app.session.me})
	match mode:
		"main": await _main()
		"stop": await _stop()
		"kill": await _kill()
	_finish()


func _main() -> void:
	var av = app.avatars.avatars[bot_key]
	var cam: Camera3D = app.camera
	var frames: Array = [] # {t, head, left, right, last_seq}
	var t0 := Time.get_ticks_msec()
	var t0_us := Time.get_ticks_usec()
	var moved := false
	var cam_a: Transform3D = cam.global_transform
	var cam_b := Transform3D.IDENTITY
	var t_move := 0.0
	var prev_t := 0.0
	var prev_pos := Vector3.ZERO
	var max_speed := 0.0
	var changed := 0
	var n := 0
	var worst_curve := 0.0
	var worst_lag := -1.0
	var best_lag_min := 99.0
	var side_ok := true
	var seen_ok := true
	var side_detail := {}
	while Time.get_ticks_msec() - t0 < 9000:
		await get_tree().process_frame
		var tsec := (Time.get_ticks_usec() - t0_us) / 1.0e6
		if not moved and tsec > 4.0:
			moved = true
			cam.position = Vector3(0.5, 1.5, 3.0)
			cam.rotation = Vector3(0.0, deg_to_rad(30.0), 0.0)
			cam_b = cam.global_transform
			t_move = Time.get_unix_time_from_system()
		var hp: Vector3 = av.head.global_position
		if n > 0:
			var dt := tsec - prev_t
			# The first frames after the avatar spawns jump from the origin to the first sample; that is
			# appearance, not motion. Judge motion once a few samples are buffered.
			if dt > 0.0 and tsec > 1.5:
				max_speed = maxf(max_speed, hp.distance_to(prev_pos) / dt)
			if hp.distance_to(prev_pos) > 0.00001:
				changed += 1
		prev_t = tsec
		var shots := OS.get_environment("POSE_TEST_SHOTS")
		if shots != "" and n % 200 == 0 and tsec > 2.0 and tsec < 3.5 or (shots != "" and n % 200 == 0 and tsec > 6.0 and tsec < 7.5):
			# Review stills: host view of the guest avatar, before and after the host moves.
			await RenderingServer.frame_post_draw
			get_viewport().get_texture().get_image().save_png("%s/host-sees-guest-%s.png" % [shots, "moved" if moved else "start"])
		prev_pos = hp
		n += 1
		if av.samples.is_empty():
			continue
		var last_seq: int = av.samples[av.samples.size() - 1]["seq"]
		# Best match of what is drawn against the bot's curve, searched around the newest sample.
		if n % 5 == 0 and tsec > 1.0:
			var hl: Vector3 = av.left.global_position
			var hr: Vector3 = av.right.global_position
			var t_last := last_seq / HZ
			var best := 99.0
			var best_t := 0.0
			var t := t_last - 0.6
			while t <= t_last + 0.1:
				var e := bot_pose(t)
				var err := maxf(hp.distance_to(e["head"]), maxf(hl.distance_to(e["left"]), hr.distance_to(e["right"])))
				if err < best:
					best = err
					best_t = t
				t += 0.002
			worst_curve = maxf(worst_curve, best)
			worst_lag = maxf(worst_lag, t_last - best_t)
			best_lag_min = minf(best_lag_min, t_last - best_t)
			# Handedness in the person's own frame: left = -x, right = +x, left raised.
			var inv: Transform3D = av.head.global_transform.affine_inverse()
			var ll: Vector3 = inv * hl
			var rl: Vector3 = inv * hr
			if not (ll.x < -0.1 and rl.x > 0.1 and ll.y > rl.y + 0.5):
				side_ok = false
			side_detail = {"left_local": str(ll), "right_local": str(rl)}
			# As the viewer sees it: the bot faces the viewer, so its left hand is on the viewer's right.
			var cinv: Transform3D = cam_a.affine_inverse() if not moved else cam_b.affine_inverse()
			var lc: Vector3 = cinv * hl
			var rc: Vector3 = cinv * hr
			if not (lc.x > rc.x):
				seen_ok = false
	_check("avatar tracks the bot's pose within 1 cm of its trajectory", worst_curve < 0.01, {"worst_m": snappedf(worst_curve, 0.0001)})
	_check("avatar trails the newest sample by at most 0.35 s (interpolation delay, no more)",
		best_lag_min > -0.03 and worst_lag < 0.35, {"min_lag_s": snappedf(best_lag_min, 0.001), "max_lag_s": snappedf(worst_lag, 0.001)})
	_check("avatar moves smoothly: no frame faster than 2 m/s, changes nearly every frame",
		max_speed < 2.0 and changed > n * 0.6, {"max_speed_mps": snappedf(max_speed, 0.01), "frames": n, "changed": changed})
	_check("left hand on the left, right on the right, left raised (person's own frame)", side_ok, side_detail)
	_check("seen from the viewer, the bot's left hand is on the viewer's right (they face each other)", seen_ok)
	var shown: String = av.label.text
	_check("name label shows the member's profile name", shown == "pose-bot", {"label": shown})
	_check("no malformed pose records", app.avatars.bad_records == 0, {"bad": app.avatars.bad_records})
	_check("published at about 20 Hz", app.publisher.sent >= 9 * 15, {"sent": app.publisher.sent})
	_drop("camera.json", {"a": _t(cam_a), "b": _t(cam_b), "t_move_unix": t_move,
		"xr": app.xr_active, "seq_sent": app.publisher.seq})


func _t(t: Transform3D) -> Dictionary:
	var q := t.basis.orthonormalized().get_rotation_quaternion()
	return {"pos": [t.origin.x, t.origin.y, t.origin.z], "rot": [q.x, q.y, q.z, q.w]}


func _stop() -> void:
	# The harness starts the bot with --stop-after; it published until then.
	var visible_while: bool = app.avatars.visible_keys().has(bot_key)
	_check("visible while the bot publishes", visible_while)
	var gone: bool = await _wait(func(): return not app.avatars.visible_keys().has(bot_key), 25.0)
	# Measured from the last record that arrived, so it does not depend on when the bot stopped.
	var since_last: float = app.avatars._now() - app.avatars.avatars[bot_key].last_rx
	_check("avatar disappears within 3 s of the last pose record", gone and since_last < 3.0, {"since_last_record_s": snappedf(since_last, 0.1)})
	await get_tree().create_timer(2.0).timeout
	_check("and stays gone", not app.avatars.visible_keys().has(bot_key))


func _kill() -> void:
	_drop("seen.json", {})
	var killed: bool = await _wait(func(): return FileAccess.file_exists(dir.path_join("killed.json")), 30.0)
	_check("harness killed the bot's daemon", killed)
	var t0 := Time.get_ticks_msec()
	var gone: bool = await _wait(func(): return not app.avatars.visible_keys().has(bot_key), 25.0)
	_check("avatar disappears after the member's daemon dies", gone, {"waited_s": snappedf((Time.get_ticks_msec() - t0) / 1000.0, 0.1)})


func _finish() -> void:
	var failures := checks.filter(func(c): return not c["pass"]).size()
	var f := FileAccess.open(dir.path_join(mode + ".report.json"), FileAccess.WRITE)
	f.store_string(JSON.stringify({"mode": mode, "checks": checks, "failures": failures}, "  "))
	f.close()
	get_tree().quit(1 if failures else 0)
