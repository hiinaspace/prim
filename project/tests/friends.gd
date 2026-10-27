extends SceneTree
# Friend list + in-room friend_request handshake, without a network. The
# "authenticated peer ID" the receiver is given models what the native
# session reports for a connection; payload ids must never matter.
const Friends = preload("res://net/friends.gd")
var failures: Array[String] = []

func check(condition: bool, message: String) -> void:
	print("CHECK ", message, ": ", condition)
	if not condition: failures.append(message)

func _initialize() -> void:
	if OS.get_name() == "Linux" and OS.get_environment("XDG_DATA_HOME").is_empty():
		printerr("Friends tests write user:// files; set XDG_DATA_HOME to an isolated test profile.")
		quit(2)
		return
	run()

func new_id() -> String:
	var identity = ClassDB.instantiate("PrimIdentity")
	return identity.key_id(identity.generate_key())

func make(id: String, file: String, display: String, outbox: Array) -> RefCounted:
	var store := Friends.new()
	store.path = "user://" + file
	DirAccess.remove_absolute(store.path)
	store.own_id = id
	store.my_name = display
	store.send = func(peer, json): outbox.append([peer, json]); return true
	return store

# Delivers everything `outbox` holds, as the native layer would: sender = authenticated id.
func deliver(sender_id: String, outbox: Array, receivers: Dictionary) -> void:
	for item in outbox:
		var message: Variant = JSON.parse_string(item[1])
		if receivers.has(item[0]): receivers[item[0]].receive(sender_id, message)
	outbox.clear()

func run() -> void:
	var identity = ClassDB.instantiate("PrimIdentity")
	var key: PackedByteArray = identity.generate_key()
	check(key.size() == 32 and identity.key_id(key) == identity.key_id(key) and not identity.key_id(key).is_empty(), "key derives a stable id")
	check(identity.key_id(PackedByteArray([1, 2, 3])) == "", "short key has no id")
	var a_id := new_id()
	var b_id := new_id()
	var c_id := new_id()
	var a_out := []
	var b_out := []
	var a := make(a_id, "friends-a.json", "Alice", a_out)
	var b := make(b_id, "friends-b.json", "Bob", b_out)
	var directory := {a_id: a, b_id: b}

	# One side asks, the other accepts: both store, using the name each saw.
	a.request(b_id, "Bobby")
	check(a.outgoing.has(b_id) and a.entries.is_empty(), "request alone does not add the requester's side")
	check(JSON.parse_string(a_out[0][1]).get("type") == "friend_request", "request message type")
	deliver(a_id, a_out, directory)
	check(b.incoming.get(a_id) == "Alice" and b.entries.is_empty(), "receiver sees a pending request, not a friend")
	check(b.accept(a_id), "accept succeeds")
	check(b.has_friend(a_id) and b.friend_name(a_id) == "Alice", "accepter stores requester under their claimed name")
	deliver(b_id, b_out, directory)
	check(a.has_friend(b_id) and a.friend_name(b_id) == "Bobby", "requester stores accepter under the name they saw")
	check(a.outgoing.is_empty() and b.incoming.is_empty(), "pending state cleared")

	# Persistence: the JSON shape is [{id,name,added}] and reloads.
	var raw: Variant = JSON.parse_string(FileAccess.get_file_as_string("user://friends-a.json"))
	check(raw is Array and raw.size() == 1 and raw[0].keys().size() == 3 and raw[0].id == b_id and raw[0].name == "Bobby" and raw[0].added is float, "friends.json shape")
	var reloaded := Friends.new()
	reloaded.path = "user://friends-a.json"
	reloaded.load_file()
	check(reloaded.has_friend(b_id) and reloaded.friend_name(b_id) == "Bobby", "friends reload after restart")
	check(reloaded.ids() == PackedStringArray([b_id]), "ids() feeds the admission set")

	# Already friends: further requests do not create pending entries.
	a.request(b_id, "Bobby")
	deliver(a_id, a_out, directory)
	check(b.incoming.is_empty() and a.entries.size() == 1 and b.entries.size() == 1, "re-request between friends is a no-op")

	# Simultaneous requests are a mutual add.
	var d_out := []
	var e_out := []
	var d_id := new_id()
	var e_id := new_id()
	var d := make(d_id, "friends-d.json", "Dee", d_out)
	var e := make(e_id, "friends-e.json", "Eve", e_out)
	var pair := {d_id: d, e_id: e}
	d.request(e_id, "Eve")
	e.request(d_id, "Dee")
	deliver(d_id, d_out, pair)
	deliver(e_id, e_out, pair)
	check(d.has_friend(e_id) and e.has_friend(d_id), "mutual requests store both sides")

	# A spoofed id in the payload is ignored: only the authenticated sender counts.
	var victim_out := []
	var victim := make(a_id, "friends-v.json", "Victim", victim_out)
	var spoof := {"type": "friend_request", "name": "Alice", "id": c_id, "from": c_id, "peer": c_id}
	victim.receive(d_id, spoof)
	check(victim.incoming.keys() == [d_id], "pending entry keyed by the authenticated sender")
	check(not victim.incoming.has(c_id) and not victim.has_friend(c_id), "payload id never becomes the sender")
	victim.accept(d_id)
	check(victim.has_friend(d_id) and not victim.has_friend(c_id), "accepting stores the authenticated sender only")
	check(not victim.receive(d_id, {"type": "other"}), "other message types are not consumed")
	# A request cannot add by itself without a local action or a matching outgoing request.
	var passive := make(a_id, "friends-p.json", "Passive", [])
	passive.receive(e_id, {"type": "friend_request", "name": "Eve"})
	check(passive.entries.is_empty(), "unsolicited request does not add a friend")
	# Requests from our own id or garbage ids are ignored.
	passive.receive(a_id, {"type": "friend_request", "name": "Me"})
	passive.receive("not-an-id", {"type": "friend_request", "name": "Bad"})
	check(passive.incoming.size() == 1, "self and malformed sender ids ignored")
	passive.peer_left(e_id)
	check(passive.incoming.is_empty(), "peer leaving clears its pending request")

	# Names are bounded, add-by-code validates and persists.
	var code_out := []
	var coder := make(a_id, "friends-c.json", "Coder", code_out)
	check(coder.add_by_code("nonsense", "x") != "", "invalid code rejected")
	check(coder.add_by_code(a_id, "x") != "", "own code rejected")
	check(coder.add_by_code("  " + c_id + "  ", "N".repeat(100)) == "", "valid code with whitespace accepted")
	check(coder.friend_name(c_id).length() == 32, "petname bounded to 32")
	check(coder.add_by_code(c_id, "again") != "", "duplicate rejected")
	check(coder.add_by_code(b_id, "") == "" and coder.friend_name(b_id) == b_id.left(8), "empty name falls back to the code prefix")
	coder.remove(c_id)
	var after := Friends.new()
	after.path = "user://friends-c.json"
	after.load_file()
	check(not after.has_friend(c_id) and after.has_friend(b_id), "remove persists")

	for file in ["a", "b", "d", "e", "v", "p", "c"]: DirAccess.remove_absolute("user://friends-%s.json" % file)
	if failures.is_empty():
		print("FRIENDS_TESTS_PASSED")
		quit()
	else:
		printerr("FRIENDS_TESTS_FAILED ", failures)
		quit(1)
