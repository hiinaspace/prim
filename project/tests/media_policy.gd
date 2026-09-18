extends SceneTree
class FakePlayer extends RefCounted:
	var duration := 0.0
	var seekable := false
	var partial := false
	var paused := true
	var position := 4.0
	var seeking := false
	var seeks: Array[float] = []
	var speed := 1.0
	func get_duration() -> float: return duration
	func get_playback_position() -> float: return position
	func get_playback_state() -> Dictionary: return {"seekable":seekable, "partially_seekable":partial, "seeking":seeking}
	func set_paused(value: bool) -> void: paused = value
	func set_playback_speed(value: float) -> void: speed = value
	func seek(value: float) -> void: seeks.append(value); seeking = true
	func is_paused() -> bool: return paused
	func is_playing() -> bool: return not paused
class FakeSession extends RefCounted:
	var host := false
	func broadcast_control(_value: String) -> void: pass
	func is_active() -> bool: return true
	func is_host() -> bool: return host
var failures: Array[String] = []
func check(ok: bool, label: String) -> void:
	print("POLICY_CHECK ", label, ": ", ok)
	if not ok: failures.append(label)
func _initialize() -> void:
	var playback := PrimPlayback.new()
	var player := FakePlayer.new()
	playback.player = player
	playback.session = FakeSession.new()
	playback.source = "https://example.invalid/live"
	playback.loaded = true
	playback.sample = {"position":1000.0, "stamp":PrimPlayback.now(), "paused":true}
	playback.desired_paused = true
	check(playback.stream_mode(), "unknown duration is treated as unbounded")
	playback.correct(true)
	check(player.seeks.is_empty() and not player.paused and player.speed == 1, "live starts without clock synchronization and ignores room pause/position")
	player.duration = 20
	check(playback.stream_mode(), "growing duration does not make unseekable media VOD")
	player.seekable = true
	player.partial = true
	check(playback.stream_mode(), "cache-only seeking is not a room movie timeline")
	player.partial = false
	check(not playback.stream_mode(), "finite seekable HTTP media restores VOD behavior")
	playback.clock_ready = true
	playback.correct(true)
	check(player.seeks == [20.0] and player.paused, "VOD resumes bounded correction and room pause")
	player.seeks.clear()
	playback.sample.live = true
	playback.correct(true)
	check(player.seeks.is_empty() and not player.paused, "host live intent overrides cache metadata")
	playback.local_only = true
	player.paused = true
	playback.correct(true)
	check(player.paused and player.seeks.is_empty(), "local-only media ignores incoming live correction")
	var malformed := {"generation":1,"revision":1,"sequence":1,"position":0,"duration":0,"stamp":0,"source":"rtsp://example.invalid/test","kind":"url","paused":false,"live":"yes"}
	check(not PrimPlayback.valid_snapshot(malformed), "malformed live intent rejected")
	malformed.live = true
	check(PrimPlayback.valid_snapshot(malformed), "boolean live snapshot accepted")
	check(PrimPlayback.is_remote_source("rtsp://c.hiina.space/test") and not PrimPlayback.is_remote_source("file:///etc/passwd"), "network source allowlist admits RTSP but not local-file URLs")
	playback.local_only = false
	playback.sample.clear()
	playback.session.host = true
	playback.resume_sample = {"position":15.0,"duration":20.0,"stamp":PrimPlayback.now(),"paused":true,"live":false}
	playback.file_loaded()
	check(playback.snapshot().position == 15 and not playback.resume_sample.is_empty(), "host keeps restored room clock while asynchronous seek is pending")
	playback.finish_room_restore()
	check(not playback.resume_sample.is_empty(), "pending restore cannot publish old player position")
	player.seeking = false
	player.position = 15
	playback.finish_room_restore()
	check(playback.resume_sample.is_empty(), "settled restore returns authority to actual player clock")
	playback.free()
	print("MEDIA_POLICY_RESULT ", JSON.stringify(failures))
	quit(0 if failures.is_empty() else 1)
