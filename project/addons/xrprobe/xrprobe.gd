## xrprobe: truthful per-eye probes + harness-owned invariants for Godot OpenXR.
## Inactive unless XRPROBE_OUT is set. Writes <out>/events.jsonl and per-eye
## thumbnails <out>/frames/<tag>-<eye>.png. Invariants live here, versioned with
## INVARIANTS_VERSION; an app or agent must not weaken one without flagging it.
class_name XRProbe
extends Node

const VERSION := "0.1.0"
const INVARIANTS_VERSION := 2
const EyeProbe := preload("res://addons/xrprobe/eye_probe.gd")
const BLACK_LUMA := 0.01

var out_dir := OS.get_environment("XRPROBE_OUT")
var sha := OS.get_environment("XRPROBE_SHA")
var probe: CompositorEffect
var camera: Camera3D
var failures := 0
var _start_ms := Time.get_ticks_msec()


func active() -> bool:
	return not out_dir.is_empty()


func _ready() -> void:
	if active():
		DirAccess.make_dir_recursive_absolute(out_dir.path_join("frames"))
		event("start", {"xrprobe": VERSION, "invariants": INVARIANTS_VERSION, "sha": sha,
			"engine": Engine.get_version_info().string,
			"renderer": RenderingServer.get_current_rendering_method(),
			"engine_path": OS.get_executable_path()})


## Appends the eye probe to `cam`'s compositor, keeping the app's own effects.
func attach(cam: Camera3D) -> void:
	camera = cam
	probe = EyeProbe.new()
	if cam.compositor == null:
		cam.compositor = Compositor.new()
	var effects := cam.compositor.compositor_effects
	effects.append(probe)
	cam.compositor.compositor_effects = effects


func event(kind: String, data := {}) -> void:
	if not active():
		return
	var rec := {"t_ms": Time.get_ticks_msec() - _start_ms, "kind": kind,
		"frame": Engine.get_frames_drawn()}
	rec.merge(data)
	var path := out_dir.path_join("events.jsonl")
	var f := FileAccess.open(path, FileAccess.READ_WRITE if FileAccess.file_exists(path) else FileAccess.WRITE)
	f.seek_end()
	f.store_line(JSON.stringify(rec))


func _scene_hash() -> String:
	var scene := get_tree().current_scene
	if scene == null or scene.scene_file_path.is_empty():
		return ""
	return "%s@%s" % [scene.scene_file_path, FileAccess.get_md5(scene.scene_file_path).left(12)]


## Boot self-check: the XR path the headset would see is actually live.
## `required` lists node paths (relative to the current scene) that must exist.
func boot_check(required: Array[String] = [], timeout_s := 20.0) -> bool:
	var xr := XRServer.find_interface("OpenXR") as OpenXRInterface
	check("openxr_initialized", xr != null and xr.is_initialized())
	if xr == null:
		return false
	var t := 0.0
	while xr.get_session_state() != OpenXRInterface.SESSION_STATE_FOCUSED and t < timeout_s:
		await get_tree().process_frame
		t += get_process_delta_time()
	check("session_focused", xr.get_session_state() == OpenXRInterface.SESSION_STATE_FOCUSED,
		{"state": xr.get_session_state()})
	check("camera_is_xr", camera is XRCamera3D and camera.current)
	check("viewport_use_xr", camera != null and camera.get_viewport().use_xr)
	for _i in 10:
		await get_tree().process_frame
	var v: Dictionary = probe.views() if probe else {}
	check("eye_probe_rendering", v.get("frame", 0) > 3, v)
	for hand in ["left_hand", "right_hand"]:
		var tr := XRServer.get_tracker(hand) as XRPositionalTracker
		var profile := tr.profile if tr else ""
		check("bound_profile_" + hand, not profile.is_empty() and profile != "/interaction_profiles/none",
			{"profile": profile})
	for p in required:
		check("node " + p, get_tree().current_scene.has_node(p))
	return failures == 0


## Records one invariant result; returns `ok`.
func check(name: String, ok: bool, detail := {}) -> bool:
	if not ok:
		failures += 1
	event("check", {"name": name, "pass": ok, "detail": detail})
	print("xrprobe: %s %s %s" % ["PASS" if ok else "FAIL", name, JSON.stringify(detail)])
	return ok


## Captures both eyes on the next frame; writes thumbnails and returns per-eye stats.
func capture(tag: String, timeout_frames := 30) -> Dictionary:
	if probe == null:
		return {}
	var head := XRServer.get_hmd_transform()
	probe.request_capture(tag)
	var cap := {}
	for _i in timeout_frames:
		await get_tree().process_frame
		cap = probe.take_capture()
		if not cap.is_empty():
			break
	if cap.is_empty():
		check("capture " + tag, false, {"reason": "no frame rendered", "probe": probe.views()})
		return {}
	var eyes := []
	var eye_cells := []
	for i in cap.eyes.size():
		eye_cells.append(cap.eyes[i].cells)
		var stats := _eye_stats(cap.eyes[i].cells)
		var png := "frames/%s-%s.png" % [tag, ["left", "right"][i] if cap.eyes.size() == 2 else "mono"]
		_thumbnail(cap.eyes[i].cells).save_png(out_dir.path_join(png))
		stats["png"] = png
		eyes.append(stats)
	var o := head.origin
	var q := head.basis.get_rotation_quaternion()
	var rec := {"tag": tag, "render_frame": cap.frame, "buffers": cap.buffers, "scene": cap.scene,
		"ipd": cap.get("ipd", 0.0), "size": [cap.size.x, cap.size.y], "sha": sha,
		"scene_hash": _scene_hash(), "head": [o.x, o.y, o.z, q.x, q.y, q.z, q.w], "eyes": eyes,
		"probe_error": cap.error}
	event("capture", rec)
	rec["eye_cells"] = eye_cells
	return rec


## Generic, convention-free invariants on one capture.
## expect: {sky: bool (environment draws a sky), world: bool (geometry should be in view)}
func check_capture(rec: Dictionary, expect := {}) -> bool:
	var before := failures
	var tag: String = rec.get("tag", "?")
	check(tag + ": render views == scene views == 2",
		rec.get("buffers") == 2 and rec.get("scene") == 2,
		{"buffers": rec.get("buffers"), "scene": rec.get("scene")})
	var eyes: Array = rec.get("eyes", [])
	for i in eyes.size():
		var e: Dictionary = eyes[i]
		var name := "%s/%s" % [tag, ["left", "right"][i] if eyes.size() == 2 else "mono"]
		check(name + ": no NaN", e.nan == 0, {"nan": e.nan})
		check(name + ": not all black", e.black_frac < 0.98, {"black_frac": e.black_frac})
		if expect.get("sky", false):
			check(name + ": background (sky) drawn, not black", e.bg_frac < 0.01 or e.bg_black_frac < 0.5,
				{"bg_frac": e.bg_frac, "bg_black_frac": e.bg_black_frac})
		if expect.get("world", false):
			check(name + ": geometry in view (not background-only)", e.bg_frac < 0.98,
				{"bg_frac": e.bg_frac})
	if eyes.size() == 2:
		var dl := absf(eyes[0].mean_luma - eyes[1].mean_luma)
		var db := absf(eyes[0].bg_frac - eyes[1].bg_frac)
		check(tag + ": eyes agree (luma, background)", dl < 0.15 and db < 0.25,
			{"d_luma": dl, "d_bg": db})
		# Near content sits further left in the right eye. A positive best shift
		# means swapped or crossed eyes; 0 (all far / flat) is inconclusive, not a fail.
		var shift := _stereo_shift(rec.eye_cells[0], rec.eye_cells[1])
		check(tag + ": stereo parallax direction (left eye is left)", shift <= 0,
			{"best_shift_cells": shift})
	return failures == before


## Horizontal shift (in thumbnail cells) of the right eye's luma that best
## matches the left eye, over shifts -12..12, by mean absolute difference.
func _stereo_shift(left: PackedFloat32Array, right: PackedFloat32Array) -> int:
	var side := int(sqrt(left.size() / 5))
	var best := 0
	var best_err := INF
	for s in range(-12, 13):
		var err := 0.0
		var n := 0
		for y in range(0, side, 2):
			for x in range(maxi(0, -s), mini(side, side - s)):
				var a := (y * side + x) * 5
				var b := (y * side + x + s) * 5
				err += absf((left[a] + left[a + 1] + left[a + 2]) - (right[b] + right[b + 1] + right[b + 2]))
				n += 1
		err /= maxf(n, 1)
		if err < best_err - 1e-6 or (absf(err - best_err) <= 1e-6 and absi(s) < absi(best)):
			best_err = err
			best = s
	return best


func _eye_stats(cells: PackedFloat32Array) -> Dictionary:
	var n := cells.size() / 5
	var black := 0
	var nan := 0
	var bg := 0.0
	var bg_cells := 0
	var bg_black := 0
	var luma_sum := 0.0
	var alpha_sum := 0.0
	for c in n:
		var r := cells[c * 5]
		var g := cells[c * 5 + 1]
		var b := cells[c * 5 + 2]
		if is_nan(r) or is_nan(g) or is_nan(b):
			nan += 1
			continue
		var luma := 0.2126 * r + 0.7152 * g + 0.0722 * b
		luma_sum += luma
		alpha_sum += cells[c * 5 + 3]
		var is_black := luma < BLACK_LUMA
		if is_black:
			black += 1
		bg += cells[c * 5 + 4]
		if cells[c * 5 + 4] > 0.5:
			bg_cells += 1
			if is_black:
				bg_black += 1
	return {"black_frac": float(black) / n, "nan": nan, "bg_frac": bg / n,
		"bg_black_frac": float(bg_black) / bg_cells if bg_cells else 0.0,
		"mean_luma": luma_sum / n, "mean_alpha": alpha_sum / n}


func _thumbnail(cells: PackedFloat32Array) -> Image:
	var side := int(sqrt(cells.size() / 5))
	var img := Image.create_empty(side, side, false, Image.FORMAT_RGBA8)
	for y in side:
		for x in side:
			var o := (y * side + x) * 5
			var c := Color(cells[o], cells[o + 1], cells[o + 2]).clamp().linear_to_srgb()
			img.set_pixel(x, y, Color(c, 1.0))
	return img
