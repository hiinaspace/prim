extends RefCounted
# Local friends list (petnames keyed by endpoint ID) and the in-room friend-request
# handshake. The ID of a request's sender is always the authenticated peer ID the
# caller passes in; ids inside payloads are ignored.

signal changed

const MAX_NAME := 32

var path := "user://friends.json"
var entries: Array[Dictionary] = []
# Peers who asked me, keyed by authenticated ID, with the name they claimed.
var incoming := {}
# Peers I asked, keyed by ID, with the name I saw them use (my petname for them).
var outgoing := {}
# send.call(peer_id: String, json: String) -> bool
var send: Callable
var my_name := "Friend"
var own_id := ""

func load_file() -> void:
	entries.clear()
	if not FileAccess.file_exists(path): return
	var parsed: Variant = JSON.parse_string(FileAccess.get_file_as_string(path))
	if not parsed is Array: return
	for item in parsed:
		if item is Dictionary and item.get("id") is String and not has_friend(item.id):
			entries.append({
				"id": item.id,
				"name": clean_name(str(item.get("name", "")), item.id),
				"added": int(item.get("added", 0)),
			})
	changed.emit()

func save_file() -> void:
	var file := FileAccess.open(path, FileAccess.WRITE)
	if file == null: return
	file.store_string(JSON.stringify(entries, "\t"))
	file.close()

static func clean_name(value: String, id: String = "") -> String:
	value = value.strip_edges().left(MAX_NAME)
	if value.is_empty(): return id.left(8) if not id.is_empty() else "Friend"
	return value

static func valid_id(id: String) -> bool:
	if ClassDB.class_exists("PrimIdentity"):
		return ClassDB.instantiate("PrimIdentity").call("is_valid_id", id)
	return id.length() == 64 and id.is_valid_hex_number()

func has_friend(id: String) -> bool:
	for entry in entries:
		if entry.id == id: return true
	return false

func ids() -> PackedStringArray:
	var result := PackedStringArray()
	for entry in entries: result.append(entry.id)
	return result

func friend_name(id: String) -> String:
	for entry in entries:
		if entry.id == id: return entry.name
	return ""

func add(id: String, name: String) -> bool:
	if has_friend(id) or id == own_id or not valid_id(id): return false
	entries.append({"id": id, "name": clean_name(name, id), "added": int(Time.get_unix_time_from_system())})
	save_file()
	changed.emit()
	return true

func remove(id: String) -> void:
	for i in range(entries.size()):
		if entries[i].id == id:
			entries.remove_at(i)
			save_file()
			changed.emit()
			return

# Paste-a-code add: one-sided, meaning "I admit this key". Returns "" or an error.
func add_by_code(code: String, name: String) -> String:
	code = code.strip_edges()
	if not valid_id(code): return "That is not a valid friend code."
	if code == own_id: return "That is your own friend code."
	if has_friend(code): return "Already in your friends list."
	add(code, name)
	return ""

# Ask a connected peer. If they already asked me this is the Accept press.
func request(peer: String, seen_name: String) -> bool:
	if peer == own_id or not valid_id(peer): return false
	if has_friend(peer):
		incoming.erase(peer)
		outgoing.erase(peer)
		return true
	if incoming.has(peer):
		add(peer, incoming[peer])
		incoming.erase(peer)
		outgoing.erase(peer)
	else:
		outgoing[peer] = clean_name(seen_name, peer)
		changed.emit()
	if send.is_valid():
		send.call(peer, JSON.stringify({"type": "friend_request", "name": my_name}))
	return true

func accept(peer: String) -> bool:
	if not incoming.has(peer): return false
	return request(peer, incoming[peer])

# Handles a control message from an authenticated peer. Returns true if consumed.
func receive(peer: String, message: Dictionary) -> bool:
	if message.get("type") != "friend_request": return false
	if peer == own_id or not valid_id(peer): return true
	var claimed := clean_name(str(message.get("name", "")), peer)
	if has_friend(peer):
		outgoing.erase(peer)
		incoming.erase(peer)
	elif outgoing.has(peer):
		# I asked first, so their request is the answer: both sides now store.
		add(peer, outgoing[peer])
		outgoing.erase(peer)
		incoming.erase(peer)
	else:
		incoming[peer] = claimed
		changed.emit()
	return true

func peer_left(peer: String) -> void:
	var dirty := incoming.erase(peer)
	if outgoing.erase(peer): dirty = true
	if dirty: changed.emit()

func clear_pending() -> void:
	if incoming.is_empty() and outgoing.is_empty(): return
	incoming.clear()
	outgoing.clear()
	changed.emit()
