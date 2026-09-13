class_name PrimAvatarExpressions
extends RefCounted

# Stable OpenLipSync order. Authored vrc.v_* shapes always take precedence.
const NAMES := ["sil", "pp", "ff", "th", "dd", "kk", "ch", "ss", "nn", "rr", "aa", "e", "ih", "oh", "ou"]
const VOWELS := [10, 12, 14, 11, 13] # aa ih ou ee oh
const ALIASES := [["aa", "a"], ["ih", "i"], ["ou", "u"], ["ee", "e"], ["oh", "o"]]
# Conservative mixtures of existing vowel deltas, inspired by CATS's technique.
# These are approximations, not newly authored consonant geometry or CATS code.
const APPROX := {
	2: [0.0, 0.30, 0.0, 0.0, 0.0], # FF: narrow opening
	3: [0.25, 0.15, 0.0, 0.0, 0.0],
	4: [0.15, 0.35, 0.0, 0.0, 0.0],
	5: [0.30, 0.15, 0.0, 0.0, 0.0],
	6: [0.0, 0.55, 0.15, 0.0, 0.0],
	7: [0.0, 0.45, 0.0, 0.0, 0.0],
	8: [0.0, 0.25, 0.0, 0.0, 0.0],
	9: [0.0, 0.15, 0.25, 0.0, 0.0],
}
var binds: Array = []
var owned: Array = []
var current := PackedFloat32Array()
var authored := PackedByteArray()

func configure(model: Node) -> void:
	reset()
	binds.clear()
	owned.clear()
	current = PackedFloat32Array()
	current.resize(15)
	authored.resize(15)
	authored.fill(0)
	for i in range(15): binds.append([])
	# Some UniVRM exports retain the raw Oculus shapes but no expression groups.
	for mesh in model.find_children("*", "MeshInstance3D", true, false):
		if not mesh.mesh: continue
		for shape in range(mesh.mesh.get_blend_shape_count()):
			var label: String = String(mesh.mesh.get_blend_shape_name(shape)).to_lower()
			for i in range(15):
				if label in ["vrc.v_" + NAMES[i], "viseme_" + NAMES[i]]:
					binds[i].append([mesh, shape, 1.0])
					authored[i] = 1
	for player in model.find_children("*", "AnimationPlayer", true, false):
		var root: Node = player.get_node(player.root_node)
		for animation_name in player.get_animation_list():
			var label: String = String(animation_name).get_slice("/", String(animation_name).count("/")).to_lower()
			for vowel in range(5):
				var index: int = VOWELS[vowel]
				if authored[index] or not label in ALIASES[vowel]: continue
				var animation: Animation = player.get_animation(animation_name)
				for track in range(animation.get_track_count()):
					# Imported vowel animations also contain eye rotations; never play them.
					if animation.track_get_type(track) != Animation.TYPE_BLEND_SHAPE or animation.track_get_key_count(track) == 0: continue
					var path: NodePath = animation.track_get_path(track)
					var mesh := root.get_node_or_null(NodePath(path.get_concatenated_names())) as MeshInstance3D
					if mesh == null or mesh.mesh == null or path.get_subname_count() != 1: continue
					var shape := mesh.find_blend_shape_by_name(path.get_subname(0))
					if shape < 0: continue
					binds[index].append([mesh, shape, clampf(float(animation.track_get_key_value(track, 0)), 0, 1)])
	for index in APPROX:
		if authored[index]: continue
		for vowel in range(5):
			var amount: float = APPROX[index][vowel]
			if amount == 0: continue
			for bind in binds[VOWELS[vowel]]: binds[index].append([bind[0], bind[1], bind[2] * amount])
	var seen := {}
	for group in binds:
		for bind in group:
			var key := str(bind[0].get_instance_id()) + ":" + str(bind[1])
			if seen.has(key): continue
			seen[key] = true
			owned.append([bind[0], bind[1], bind[0].get_blend_shape_value(bind[1])])

func reset() -> void:
	current.fill(0.0)
	for bind in owned:
		if is_instance_valid(bind[0]): bind[0].set_blend_shape_value(bind[1], bind[2])

func apply(weights: PackedFloat32Array, delta: float) -> void:
	if weights.size() != 15 or weights == PackedFloat32Array([0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]):
		reset()
		return
	var closure := maxf(safe(weights[0]), safe(weights[1]))
	for i in range(15):
		var target := safe(weights[i])
		if i > 1: target *= 1.0 - closure
		# One smoothing stage, independent of audio chunk/render rate.
		current[i] = lerpf(current[i], target, 1.0 - exp(-maxf(delta, 0) * (45.0 if target > current[i] else 35.0)))
		if target == 0 and current[i] < 0.001: current[i] = 0
	var baselines := {}
	var totals := {}
	for bind in owned:
		if is_instance_valid(bind[0]):
			if not totals.has(bind[0]): totals[bind[0]] = {}
			totals[bind[0]][bind[1]] = 0.0
			if not baselines.has(bind[0]): baselines[bind[0]] = {}
			baselines[bind[0]][bind[1]] = bind[2]
	for i in range(15):
		for bind in binds[i]:
			if is_instance_valid(bind[0]): totals[bind[0]][bind[1]] += current[i] * bind[2]
	# Limit summed vowel deformation, preserving authored relative weights.
	for mesh in totals:
		var sum := 0.0
		for value in totals[mesh].values(): sum += value
		var scale := 1.0 / maxf(1.0, sum)
		for shape in totals[mesh]: mesh.set_blend_shape_value(shape, clampf(baselines[mesh][shape] + totals[mesh][shape] * scale, 0, 1))

static func safe(value: float) -> float:
	return clampf(value, 0, 1) if is_finite(value) else 0.0
