## One read-only Background helper, with bounded request/reply IPC.
extends RefCounted

const HOME_KEY := "openvr.tool.steamvr_environments"
const MAX_AGE_MS := 500
var process := {}
var pending := false
var sequence := 0
var sent_at := 0
var received_at := -10000
var started_at := 0
var buffer := ""
var latest := {}
var error := ""

func start() -> bool:
	stop()
	error = ""
	var platform := "windows" if OS.get_name() == "Windows" else "linux"
	var name := "prim-openvr-helper" + (".exe" if platform == "windows" else "")
	var path := ProjectSettings.globalize_path("res://bin/" + platform + "/" + name)
	var packaged := OS.get_executable_path().get_base_dir().path_join(name if platform == "windows" else "tools/" + name)
	if FileAccess.file_exists(packaged): path = packaged
	if not FileAccess.file_exists(path):
		error = "SteamVR tracking helper is missing. Rebuild Prim."
		return false
	# Portable Linux uses a private loader, also for child executables.
	var arguments := PackedStringArray()
	if OS.get_name() == "Linux":
		var root := OS.get_executable_path().get_base_dir()
		var loader := root.path_join("lib/ld-linux-x86-64.so.2")
		if FileAccess.file_exists(loader):
			var libraries := OS.get_environment("PRIM_RUNTIME_LIBRARY_PATH")
			if libraries.is_empty(): libraries = root.path_join("lib") + ":" + OS.get_environment("LD_LIBRARY_PATH")
			arguments = PackedStringArray(["--library-path", libraries, path])
			path = loader
	process = OS.execute_with_pipe(path, arguments, false)
	started_at = Time.get_ticks_msec()
	if process.is_empty(): error = "Could not launch SteamVR tracking helper."
	return not process.is_empty()

func stop() -> void:
	if not process.is_empty():
		# Closing stdin asks the helper to exit even if Prim itself terminates.
		process.stdio.close()
		process.stderr.close()
		if OS.is_process_running(process.pid): OS.kill(process.pid)
	process = {}
	latest = {}
	pending = false
	buffer = ""
	received_at = -10000

func _notification(what: int) -> void:
	if what == NOTIFICATION_PREDELETE and not process.is_empty():
		process.stdio.close()
		process.stderr.close()
		if OS.is_process_running(process.pid): OS.kill(process.pid)

func poll() -> void:
	if process.is_empty(): return
	var io: FileAccess = process.stdio
	var bytes := mini(int(io.get_length()), 65536)
	if bytes > 0: buffer += io.get_buffer(bytes).get_string_from_utf8()
	# OpenVR may print diagnostics on either pipe. Drain stderr without retaining
	# unbounded output; the helper's protocol carries actionable initialization errors.
	var stderr: FileAccess = process.stderr
	var stderr_bytes := mini(int(stderr.get_length()), 65536)
	if stderr_bytes > 0: stderr.get_buffer(stderr_bytes)
	if buffer.length() > 131072:
		error = "SteamVR helper output exceeded its limit."
		stop()
		return
	while "\n" in buffer:
		var end := buffer.find("\n")
		var line := buffer.substr(0, end)
		buffer = buffer.substr(end + 1)
		if not line.begins_with("PRIM_OPENVR "): continue
		var sample = JSON.parse_string(line.substr(12))
		if not sample is Dictionary: continue
		if sample.has("error"):
			error = str(sample.error)
			stop()
			return
		if pending and int(sample.get("seq", -1)) == sequence:
			pending = false
			if Time.get_ticks_msec() - sent_at <= MAX_AGE_MS and valid_snapshot(sample):
				latest = sample
				received_at = sent_at # Conservative age includes pipe round-trip.
	if not OS.is_process_running(process.pid):
		error = "SteamVR tracking helper disconnected."
		stop()
		return
	if latest.is_empty() and Time.get_ticks_msec() - started_at > 15000:
		error = "SteamVR did not answer. Check the headset/runtime and enable VR again."
		stop()
		return
	if not latest.is_empty() and pending and Time.get_ticks_msec() - sent_at > 5000:
		error = "SteamVR tracking stopped responding."
		stop()
		return
	if not pending:
		sequence += 1
		sent_at = Time.get_ticks_msec()
		io.store_string("poll %d\n" % sequence)
		pending = true

static func valid_snapshot(sample: Dictionary) -> bool:
	var poses = sample.get("poses")
	if not poses is Array or poses.size() != 3: return false
	for pose in poses:
		if not pose is Dictionary: return false
		var matrix = pose.get("matrix")
		if not matrix is Array or matrix.size() != 12: return false
		for value in matrix:
			if not (value is float or value is int) or not is_finite(float(value)): return false
	return sample.has("scene_pid") and sample.has("scene_state")

func snapshot() -> Dictionary:
	var result := latest.duplicate()
	result["age_ms"] = Time.get_ticks_msec() - received_at
	result["runtime"] = "SteamVR"
	result["activity"] = classify(result, OS.get_process_id())
	return result

static func classify(sample: Dictionary, own_pid: int) -> String:
	if int(sample.get("age_ms", MAX_AGE_MS + 1)) > MAX_AGE_MS: return "unknown"
	var pid := int(sample.get("scene_pid", -1))
	var state := int(sample.get("scene_state", -1))
	if state < 0 or state > 4: return "unknown"
	if state == 1 or state == 2 or not str(sample.get("starting", "")).is_empty(): return "transition"
	if pid == own_pid: return "self"
	if int(sample.get("starting_error", -1)) not in [0, 102]: return "unknown"
	if pid == 0: return "idle" if state == 0 else "unknown"
	if pid < 0: return "unknown"
	if int(sample.get("key_error", -1)) == 0 and str(sample.get("key", "")) == HOME_KEY: return "idle"
	return "other" # A live unknown process is still a game, even behind dashboard.

static func pose_transform(pose: Dictionary) -> Transform3D:
	var m: Array = pose.matrix
	return Transform3D(Basis(Vector3(m[0],m[4],m[8]), Vector3(m[1],m[5],m[9]), Vector3(m[2],m[6],m[10])), Vector3(m[3],m[7],m[11]))
