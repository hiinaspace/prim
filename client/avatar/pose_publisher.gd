extends Node
## Publishes this client's head and hands as `member/<me>/pose` (live record, 20 Hz,
## PrimPose.encode). Head = the camera (headset or desktop view) in room coordinates;
## hands = the two controllers, flagged tracked only while the runtime reports them.
## A desktop player has no hands: they are sent resting beside the head with the tracked
## flags off, so viewers hide them. Needs a node that exposes camera, xr_active and
## hand_nodes (main.gd).

const HZ := 20.0

var client: PrimClient
var rig # main.gd: camera, xr_active, hand_nodes {"left": XRController3D, "right": ...}
var ready_fn: Callable # () -> bool: only publish once in a room
var seq := 0
var sent := 0
var _acc := 0.0
var enabled := true
var log_path := "" # test hook: append what was sent (camera transform at send time)


func setup(c: PrimClient, r, ready_callable: Callable) -> void:
	client = c
	rig = r
	ready_fn = ready_callable
	name = "PosePublisher"


func current() -> Dictionary:
	var head: Transform3D = rig.camera.global_transform
	var flags := PrimPose.FLAG_VR if rig.xr_active else 0
	var l := head * Transform3D(Basis.IDENTITY, Vector3(-0.3, -0.5, -0.2))
	var r := head * Transform3D(Basis.IDENTITY, Vector3(0.3, -0.5, -0.2))
	for side in ["left", "right"]:
		var node: XRController3D = rig.hand_nodes.get(side)
		if node != null and node.get_has_tracking_data():
			if side == "left":
				l = node.global_transform
				flags |= PrimPose.FLAG_LEFT
			else:
				r = node.global_transform
				flags |= PrimPose.FLAG_RIGHT
	return {"head": head, "left": l, "right": r, "flags": flags}


func _process(dt: float) -> void:
	if not enabled or client == null or not ready_fn.call():
		return
	_acc += dt
	if _acc < 1.0 / HZ:
		return
	_acc = fmod(_acc, 1.0 / HZ)
	var p := current()
	var bytes := PrimPose.encode(p["flags"], seq, p["head"], p["left"], p["right"])
	if client.write_state("member", "pose", true, false, bytes) >= 0:
		sent += 1
		seq = (seq + 1) & 0xffff
