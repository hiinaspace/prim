## JSON-lines state socket for harness scenarios (127.0.0.1 only).
## Requests:  {"id": n, "op": "get", "probes": ["name", ...]}
##            {"id": n, "op": "call", "action": "name", "args": [...]}
##            {"id": n, "op": "capture", "tag": "t", "expect": {...}}
##            {"id": n, "op": "frames"}
## Replies:   {"id": n, "ok": true, "value": ...} or {"id": n, "ok": false, "error": "..."}
## Probes/actions are registered by the app glue; there is no eval.
extends Node

var xrprobe: Node
var probes := {}
var actions := {}
var _server := TCPServer.new()
var _peers: Array[StreamPeerTCP] = []
var _buffers: Array[String] = []


func listen(port: int) -> Error:
	process_mode = Node.PROCESS_MODE_ALWAYS
	var err := _server.listen(port, "127.0.0.1")
	print("xrprobe: state socket 127.0.0.1:%d err=%d" % [port, err])
	return err


func _process(_dt: float) -> void:
	while _server.is_connection_available():
		_peers.append(_server.take_connection())
		_buffers.append("")
	for i in range(_peers.size() - 1, -1, -1):
		var peer := _peers[i]
		peer.poll()
		if peer.get_status() != StreamPeerTCP.STATUS_CONNECTED:
			_peers.remove_at(i)
			_buffers.remove_at(i)
			continue
		var n := peer.get_available_bytes()
		if n > 0:
			_buffers[i] += peer.get_utf8_string(n)
		while "\n" in _buffers[i]:
			var line := _buffers[i].get_slice("\n", 0)
			_buffers[i] = _buffers[i].substr(line.length() + 1)
			_handle(peer, line)


func _handle(peer: StreamPeerTCP, line: String) -> void:
	var req = JSON.parse_string(line)
	if not req is Dictionary:
		_reply(peer, {"ok": false, "error": "bad json"})
		return
	var rep := {"id": req.get("id"), "ok": true}
	match str(req.get("op")):
		"get":
			var out := {}
			for name in req.get("probes", probes.keys()):
				out[name] = to_json(probes[name].call()) if probes.has(name) else null
			rep["value"] = out
		"call":
			var name := str(req.get("action"))
			if not actions.has(name):
				rep = {"id": req.get("id"), "ok": false, "error": "no action " + name}
			else:
				rep["value"] = to_json(await actions[name].callv(req.get("args", [])))
		"capture":
			var rec: Dictionary = await xrprobe.capture(str(req.get("tag", "capture")))
			var ok: bool = xrprobe.check_capture(rec, req.get("expect", {})) if not rec.is_empty() else false
			rec.erase("eye_cells")
			rep["value"] = {"pass": ok, "capture": rec}
		"frames":
			rep["value"] = {"drawn": Engine.get_frames_drawn(),
				"physics": Engine.get_physics_frames(), "ticks_ms": Time.get_ticks_msec()}
		_:
			rep = {"id": req.get("id"), "ok": false, "error": "unknown op"}
	_reply(peer, rep)


func _reply(peer: StreamPeerTCP, rep: Dictionary) -> void:
	peer.put_data((JSON.stringify(rep) + "\n").to_utf8_buffer())


## Godot math types -> plain JSON. Transforms become {"origin": [...], "basis": [x, y, z]}.
static func to_json(v: Variant) -> Variant:
	match typeof(v):
		TYPE_VECTOR2, TYPE_VECTOR2I:
			return [v.x, v.y]
		TYPE_VECTOR3, TYPE_VECTOR3I:
			return [v.x, v.y, v.z]
		TYPE_QUATERNION:
			return [v.x, v.y, v.z, v.w]
		TYPE_BASIS:
			return [to_json(v.x), to_json(v.y), to_json(v.z)]
		TYPE_TRANSFORM3D:
			return {"origin": to_json(v.origin), "basis": to_json(v.basis)}
		TYPE_ARRAY, TYPE_PACKED_FLOAT32_ARRAY, TYPE_PACKED_VECTOR3_ARRAY, TYPE_PACKED_INT32_ARRAY:
			return Array(v).map(func(e): return to_json(e))
		TYPE_DICTIONARY:
			var d := {}
			for k in v:
				d[str(k)] = to_json(v[k])
			return d
		TYPE_OBJECT:
			return str(v)
	return v
