extends SceneTree
var failures := []
const Gesture = preload("res://xr/reveal_gesture.gd")
var head := Transform3D(Basis.IDENTITY, Vector3(0, 1.6, 0))
var hand := Transform3D(Basis.IDENTITY, Vector3(0.15, 1.6, -0.25))
var gesture = Gesture.new()
func _initialize() -> void: run.call_deferred()
func check(value: bool, message: String) -> void:
	print("CHECK ", message, ": ", value)
	if not value: failures.append(message)
func tick(dt: float, height: float, grip: bool, valid := true, which := 0) -> float:
	var pose := hand
	pose.origin.y = head.origin.y + height
	var hands := [hand, hand]
	var tracked := [false, false]
	var held := [false, false]
	hands[which] = pose
	tracked[which] = valid
	held[which] = grip
	return gesture.update(dt, head, true, hands, tracked, held)
func arm(which := 0) -> void:
	tick(0.55, 0, false, true, which)
	tick(0.01, 0, true, true, which)
func run() -> void:
	gesture.reset()
	tick(0.3, 0, true)
	tick(0.1, 0.3, true)
	check(gesture.fraction == 0 and gesture.owner == -1, "held grip cannot arm on entry")
	tick(0.01, 0, false)
	tick(0.1, 0, true)
	check(gesture.owner == -1, "short dwell cannot arm")
	tick(0.55, 0, false)
	check(gesture.haptic_amplitudes[0] == 0.2, "dwell completion pulses ready hand")
	tick(0.1, 0, false)
	check(gesture.haptic_amplitudes[0] == 0, "ready pulse is not repeated during dwell")
	tick(0.01, 0, true)
	check(gesture.owner == 0, "dwell plus fresh grip arms left")
	check(tick(0.1, Gesture.LIFT * 0.5, true) > 0.4 and gesture.fraction < 0.6, "half lift gives continuous peek")
	check(gesture.haptic_amplitudes[0] > 0, "moving lift produces active haptic")
	var moving_amplitude: float = gesture.haptic_amplitudes[0]
	tick(0.1, Gesture.LIFT * 0.6, true)
	check(gesture.haptic_amplitudes[0] > 0 and gesture.haptic_amplitudes[0] < moving_amplitude, "slower movement produces weaker haptic")
	tick(0.1, Gesture.LIFT * 0.6, true)
	check(gesture.haptic_amplitudes[0] == 0, "stationary held gesture does not vibrate")
	head.origin.y += 0.2
	tick(0.1, Gesture.LIFT * 0.6, true)
	check(gesture.haptic_amplitudes[0] == 0, "shared head and hand motion does not vibrate")
	head.origin.y -= 0.2
	check(tick(0.1, Gesture.LIFT, true) == 1, "full held lift reveals room")
	tick(5.0, Gesture.LIFT, true)
	check(gesture.fraction == 1, "completed held peek does not time out")
	tick(0.1, 0, true)
	tick(0.01, 0, false)
	check(gesture.haptic_amplitudes[0] == 0, "release stops haptic requests")
	check(gesture.fraction == 0 and not gesture.latched, "lower and release closes quick peek")
	arm(1)
	tick(0.1, Gesture.LIFT, true, true, 1)
	tick(0.01, Gesture.LIFT, false, true, 1)
	check(gesture.latched and gesture.fraction == 1, "right hand release at top latches")
	tick(0.3, Gesture.LIFT, false)
	tick(0.01, Gesture.LIFT, true)
	check(tick(0.01, Gesture.LIFT, true) > 0.99, "closing starts fully revealed at reduced lift height")
	tick(0.1, 0.07, true)
	tick(0.01, 0.07, false)
	check(gesture.latched, "incomplete lowering restores latch")
	tick(0.3, Gesture.LIFT, false)
	tick(0.01, Gesture.LIFT, true)
	tick(0.1, 0, true)
	tick(0.01, 0, false)
	check(not gesture.latched and gesture.fraction == 0, "either hand closes latch")
	arm()
	tick(0.1, Gesture.LIFT * 0.5, true)
	tick(0.01, Gesture.LIFT * 0.5, false)
	check(not gesture.latched and gesture.fraction == 0, "partial opening release cancels")
	arm()
	tick(0.1, Gesture.LIFT, true)
	tick(0.01, Gesture.LIFT, true, false)
	check(gesture.haptic_amplitudes[0] == 0, "tracking loss stops haptic requests")
	check(gesture.fraction == 0 and gesture.owner == -1, "hand loss cancels temporary peek")
	arm()
	tick(0.1, Gesture.LIFT, true)
	tick(0.01, Gesture.LIFT, false)
	gesture.update(0.01, head, false, [hand,hand], [true,true], [false,false])
	check(gesture.fraction == 0 and not gesture.latched, "head loss hides latched view")
	print("XR_GESTURE_RESULT ", JSON.stringify(failures))
	quit(0 if failures.is_empty() else 1)
