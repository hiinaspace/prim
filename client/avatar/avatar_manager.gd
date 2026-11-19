extends Node3D
## Draws every other member at the pose they publish. Owns its own `member/*/pose`
## subscription, so it survives scene hot reloads and works the same whoever started
## the daemon. An avatar is hidden when its pose stops arriving (STALE_S), when the
## daemon removes the record, when the member goes offline or leaves, and after a daemon
## reconnect until a new pose comes in.

const AvatarScript := preload("res://avatar/avatar.gd")
const STALE_S := 2.0
const COLORS := [Color(1.0, 0.65, 0.15), Color(0.2, 0.85, 1.0), Color(0.85, 0.3, 0.95), Color(0.5, 1.0, 0.4)]

signal sample(key: String, head: Vector3) # for the model/verify: a pose arrived

var client: PrimClient
var me_fn: Callable # () -> own member key
var name_fn: Callable # (key) -> display name
var avatars := {} # key -> avatar
var sub := -1
var bad_records := 0
var received := 0
var stale_s := STALE_S


func setup(c: PrimClient, me_callable: Callable, name_callable: Callable) -> void:
	client = c
	me_fn = me_callable
	name_fn = name_callable
	name = "Avatars"
	sub = client.subscribe("member/*/pose")
	client.state_value.connect(_on_value)
	client.state_removed.connect(_on_removed)
	client.state_reset.connect(_on_reset)
	client.member_event.connect(_on_member_event)


func _now() -> float:
	return Time.get_ticks_usec() / 1e6


func _on_value(s: int, path: String, writer: String, _live: bool, value: PackedByteArray) -> void:
	if s != sub:
		return
	var parts := path.split("/")
	if parts.size() != 3 or parts[0] != "member" or parts[2] != "pose":
		return
	if writer == str(me_fn.call()):
		return
	var d: Dictionary = PrimPose.decode(value)
	if d.is_empty():
		bad_records += 1
		return
	received += 1
	var av = avatars.get(writer)
	if av == null:
		av = AvatarScript.new()
		av.setup(writer, COLORS[hash(writer) % COLORS.size()])
		avatars[writer] = av
		add_child(av)
	av.push(_now(), d)
	sample.emit(writer, (d["head"] as Transform3D).origin)


func _on_removed(s: int, path: String) -> void:
	if s != sub:
		return
	var parts := path.split("/")
	if parts.size() == 3 and avatars.has(parts[1]):
		_drop(parts[1])


func _on_reset(s: int) -> void:
	if s == sub:
		for k in avatars.keys():
			_drop(k)


func _on_member_event(key: String, what: String) -> void:
	if what in ["offline", "removed"] and avatars.has(key):
		_drop(key)


func _drop(key: String) -> void:
	var av = avatars.get(key)
	if av:
		av.queue_free()
		avatars.erase(key)


func _process(_dt: float) -> void:
	var now := _now()
	for key in avatars:
		var av = avatars[key]
		var alive: bool = av.last_rx >= 0.0 and now - av.last_rx < stale_s
		av.visible = alive
		if alive:
			av.update(now)
			av.set_label(str(name_fn.call(key)) if str(name_fn.call(key)) != "" else key.left(6))


## The head position the avatar is drawn at now, or null when it is not shown.
func head_position(key: String) -> Variant:
	var av = avatars.get(key)
	return av.head.global_position if av and av.visible else null


func visible_keys() -> Array:
	return avatars.keys().filter(func(k): return avatars[k].visible)
