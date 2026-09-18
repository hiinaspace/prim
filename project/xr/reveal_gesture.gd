## Physical pose recognizer. No input is captured from the other application.
extends RefCounted

const DWELL := 0.5
const LIFT := 0.11
const CLOSE_HEIGHT := 0.04
var dwell := [0.0, 0.0]
var previous_grip := [false, false]
var owner := -1
var opening := true
var latched := false
var fraction := 0.0
var elapsed := 0.0
var start_height := 0.0
var cooldown := 0.0
var reached_top := false
# Per-update pulse requests; the caller sends these through the controller action.
var haptic_amplitudes := [0.0, 0.0]
var previous_height := 0.0
var haptic_cooldown := 0.0

func reset() -> void:
	dwell = [0.0, 0.0]
	previous_grip = [true, true] # Release before rearming, including after game transitions.
	owner = -1
	latched = false
	fraction = 0.0
	cooldown = 0.2
	elapsed = 0.0
	haptic_amplitudes = [0.0, 0.0]
	haptic_cooldown = 0.0

func update(delta: float, head: Transform3D, head_valid: bool, hands: Array, valid: Array, held: Array) -> float:
	haptic_amplitudes = [0.0, 0.0]
	haptic_cooldown = maxf(0, haptic_cooldown - delta)
	cooldown = maxf(0, cooldown - delta)
	if not head_valid:
		reset()
		return 0.0
	if owner >= 0:
		elapsed += delta
		if not valid[owner] or (elapsed > 3.0 and not reached_top):
			_finish(latched)
		else:
			var height: float = hands[owner].origin.y - head.origin.y
			var lift := clampf((height - start_height) / LIFT, 0, 1) if opening else clampf((height - CLOSE_HEIGHT) / maxf(start_height - CLOSE_HEIGHT, 0.01), 0, 1)
			# Relative height ignores shared head/hand translation (walking or WASD).
			var speed := absf(height - previous_height) / maxf(delta, 0.001)
			previous_height = height
			if held[owner] and haptic_cooldown <= 0 and speed > 0.03:
				haptic_amplitudes[owner] = clampf(speed * 0.5, 0.0, 0.35)
				haptic_cooldown = 0.04
			fraction = lift
			if fraction >= 0.95: reached_top = true
			if not held[owner]:
				_finish(fraction >= 0.9 if opening else fraction > 0.1)
	else:
		for i in range(2):
			if not valid[i]:
				dwell[i] = 0.0
				continue
			var relative: Vector3 = head.affine_inverse() * hands[i].origin
			var near_face: bool = relative.z < -0.08 and relative.z > -0.48 and absf(relative.x) < 0.30 and absf(relative.y) < 0.25
			var height: float = hands[i].origin.y - head.origin.y
			var near_top: bool = height > 0.06 and height < 0.45 and Vector2(relative.x, relative.z).length() < 0.5
			var fresh: bool = held[i] and not previous_grip[i]
			if cooldown <= 0 and fresh and ((not latched and near_face and dwell[i] >= DWELL) or (latched and near_top)):
				owner = i
				opening = not latched
				start_height = height
				previous_height = height
				haptic_cooldown = 0.0
				elapsed = 0.0
				reached_top = latched
				dwell = [0.0, 0.0]
				break
			if near_face and not held[i]:
				var before: float = dwell[i]
				dwell[i] += delta
				if not latched and cooldown <= 0 and before < DWELL and dwell[i] >= DWELL:
					haptic_amplitudes[i] = 0.2
			else: dwell[i] = 0.0
	previous_grip = held.duplicate()
	return fraction

func _finish(keep_open: bool) -> void:
	latched = keep_open
	fraction = 1.0 if latched else 0.0
	owner = -1
	dwell = [0.0, 0.0]
	cooldown = 0.2
