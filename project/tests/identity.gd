extends SceneTree
# Identity persistence, Friends tab and instance hosting through the real main scene.
# Run with an isolated XDG_DATA_HOME and PRIM_NETWORK_LOCAL_ONLY=1.
var app
var failures: Array[String] = []

func check(condition: bool, message: String) -> void:
	print("CHECK ", message, ": ", condition)
	if not condition: failures.append(message)

func _initialize() -> void:
	if OS.get_name() == "Linux" and OS.get_environment("XDG_DATA_HOME").is_empty():
		printerr("Identity tests write user:// files; set XDG_DATA_HOME to an isolated test profile.")
		quit(2)
		return
	if OS.get_environment("PRIM_NETWORK_LOCAL_ONLY") != "1":
		printerr("Set PRIM_NETWORK_LOCAL_ONLY=1 so the hosted instance never touches the public network.")
		quit(2)
		return
	call_deferred("run")

func wait_for(predicate: Callable, seconds := 10.0) -> bool:
	var deadline := Time.get_ticks_msec() + int(seconds * 1000)
	while not predicate.call() and Time.get_ticks_msec() < deadline:
		await process_frame
	return predicate.call()

func press(parent: Node, text: String) -> Button:
	for node in parent.find_children("*", "Button", true, false):
		if node.text == text and node.is_visible_in_tree():
			node.pressed.emit()
			return node
	return null

func start() -> void:
	app = load("res://main.tscn").instantiate()
	root.add_child(app)
	await create_timer(1.0).timeout

func stop() -> void:
	root.remove_child(app)
	app.free()
	await process_frame

func run() -> void:
	root.gui_embed_subwindows = true
	for file in ["identity.key", "friends.json"]: DirAccess.remove_absolute("user://" + file)
	await start()
	var first_key: PackedByteArray = FileAccess.get_file_as_bytes("user://identity.key")
	check(first_key.size() == 32, "first run creates a 32-byte identity.key")
	check(FileAccess.get_unix_permissions("user://identity.key") == 0x180, "identity.key is owner-only")
	var first_id: String = app.identity_id
	check(not first_id.is_empty() and first_id == ClassDB.instantiate("PrimIdentity").key_id(first_key), "friend code derives from the key")
	await stop()
	await start()
	check(app.identity_id == first_id and FileAccess.get_file_as_bytes("user://identity.key") == first_key, "restart keeps the same identity")
	check(app.menu.own_code.text == first_id, "Friends tab shows own friend code")
	app.menu.select_tab("Friends")
	await process_frame
	check(app.menu.tabs.get_current_tab_control().name == "Friends", "Friends tab present")

	# Add by code through the real widgets.
	var friend_id: String = ClassDB.instantiate("PrimIdentity").key_id(ClassDB.instantiate("PrimIdentity").generate_key())
	app.menu.code_input.text = "garbage"
	press(app.menu.tabs.get_current_tab_control(), "Add")
	check(not app.menu.friend_notice.text.is_empty() and app.menu.friend_notice.text != "Added." and app.friends.entries.is_empty(), "bad code is refused with a message")
	app.menu.code_input.text = friend_id
	app.menu.friend_name_input.text = "Zed"
	press(app.menu.tabs.get_current_tab_control(), "Add")
	await process_frame
	await process_frame
	check(app.friends.has_friend(friend_id) and app.friends.friend_name(friend_id) == "Zed", "code add stores the petname")
	check(app.menu.code_input.text.is_empty() and app.menu.friend_notice.text == "Added.", "form resets after add")
	check(app.menu.friend_rows.find_children("*", "Label", true, false).any(func(l): return l.text.begins_with("Zed")), "friend row listed")
	var stored: Variant = JSON.parse_string(FileAccess.get_file_as_string("user://friends.json"))
	check(stored is Array and stored.size() == 1 and stored[0].id == friend_id, "friends.json written")
	var join_button: Button = null
	for node in app.menu.friend_rows.find_children("*", "Button", true, false):
		if node.text == "Join": join_button = node
	check(join_button != null and not join_button.disabled, "Join is offered next to the friend")

	# Host an Invite instance: the endpoint ID is the persistent identity; Invite ticks are live.
	app.menu.select_tab("Room")
	await process_frame
	press(app.menu.tabs.get_current_tab_control(), "Host Invite instance")
	check(app.hosting_mode == "invite" and app.session.is_active(), "Host Invite instance starts a hosted session")
	check(await wait_for(func(): return app.session.get_local_id() != ""), "hosted endpoint came up")
	check(app.session.get_local_id() == first_id, "hosted endpoint uses the persistent identity")
	check(await wait_for(func(): return app.session.is_host()), "host marks itself host")
	check(app.admission_ids().is_empty(), "Invite instance admits nobody until ticked")
	await process_frame
	await process_frame
	var invite: CheckButton = null
	for node in app.menu.friend_rows.find_children("*", "CheckButton", true, false): invite = node
	check(invite != null, "Invite toggle appears while hosting an Invite instance")
	if invite: invite.button_pressed = true
	check(app.invited.has(friend_id) and app.admission_ids() == PackedStringArray([friend_id]), "ticking Invite admits that friend")
	check(app.menu.host_friends_button.disabled and app.menu.host_invite_button.disabled, "host buttons disabled while connected")
	await process_frame
	for node in app.menu.friend_rows.find_children("*", "Button", true, false):
		if node.text == "Join": join_button = node
	check(join_button.disabled, "Join disabled while connected")
	app.toggle_connection()
	check(await wait_for(func(): return not app.session.is_active()), "leaving ends the hosted instance")
	check(app.hosting_mode.is_empty() and app.invited.is_empty(), "hosting state reset")
	press(app.menu.tabs.get_current_tab_control(), "Host Friends instance")
	check(app.hosting_mode == "friends" and app.admission_ids() == PackedStringArray([friend_id]), "Friends instance admits the whole friends list")
	app.toggle_connection()
	await wait_for(func(): return not app.session.is_active())

	# Removal persists.
	app.menu.select_tab("Friends")
	await process_frame
	await process_frame
	press(app.menu.friend_rows, "Remove")
	check(not app.friends.has_friend(friend_id) and JSON.parse_string(FileAccess.get_file_as_string("user://friends.json")) == [], "Remove deletes locally and persists")
	for file in ["identity.key", "friends.json"]: DirAccess.remove_absolute("user://" + file)
	if failures.is_empty():
		print("IDENTITY_TESTS_PASSED")
		quit()
	else:
		printerr("IDENTITY_TESTS_FAILED ", failures)
		quit(1)
