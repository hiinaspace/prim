extends CanvasLayer
## C1's minimal desktop screen over RoomSession: host, paste-an-invite join, who am I,
## invite + copy, roster, leave. U1 replaces this with the real menu. Controls are
## public members so tests can drive the same buttons a person clicks.

var session
var panel: PanelContainer
var title: Label
var identity: Label
var name_edit: LineEdit
var name_button: Button
var host_button: Button
var token_edit: LineEdit
var paste_button: Button
var join_button: Button
var cancel_button: Button
var invite_button: Button
var invite_edit: LineEdit
var copy_button: Button
var leave_button: Button
var mic_button: CheckButton
var deafen_button: CheckButton
var quit_button: Button
var status: Label
var error_label: Label
var roster: RichTextLabel
var _choose_box: VBoxContainer
var _pending_box: VBoxContainer
var _room_box: VBoxContainer


func setup(s) -> void:
	session = s
	layer = 10
	panel = PanelContainer.new()
	add_child(panel)
	# Top right: the loop's own roster label (room_view.gd) owns the top left.
	panel.set_anchors_preset(Control.PRESET_TOP_RIGHT)
	panel.offset_left = -456
	panel.offset_right = -16
	panel.offset_top = 16
	panel.grow_horizontal = Control.GROW_DIRECTION_BEGIN
	var root := VBoxContainer.new()
	root.add_theme_constant_override("separation", 8)
	panel.add_child(root)
	title = Label.new()
	title.add_theme_font_size_override("font_size", 22)
	root.add_child(title)
	identity = Label.new()
	root.add_child(identity)
	var nrow := HBoxContainer.new()
	root.add_child(nrow)
	name_edit = LineEdit.new()
	name_edit.placeholder_text = "Display name"
	name_edit.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	name_edit.max_length = 40
	nrow.add_child(name_edit)
	name_button = Button.new()
	name_button.text = "Set name"
	name_button.pressed.connect(func(): session.set_display_name(name_edit.text))
	name_edit.text_submitted.connect(func(t): session.set_display_name(t))
	nrow.add_child(name_button)
	error_label = Label.new()
	error_label.add_theme_color_override("font_color", Color(1.0, 0.45, 0.4))
	error_label.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	root.add_child(error_label)
	status = Label.new()
	status.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	root.add_child(status)

	_choose_box = VBoxContainer.new()
	_choose_box.add_theme_constant_override("separation", 8)
	root.add_child(_choose_box)
	host_button = Button.new()
	host_button.text = "Host a room"
	host_button.pressed.connect(func():
		if name_edit.text.strip_edges() != "":
			session.set_display_name(name_edit.text)
		session.host_room())
	_choose_box.add_child(host_button)
	_choose_box.add_child(HSeparator.new())
	var jl := Label.new()
	jl.text = "Join a friend's room: paste the invite they sent you"
	_choose_box.add_child(jl)
	var jrow := HBoxContainer.new()
	_choose_box.add_child(jrow)
	token_edit = LineEdit.new()
	token_edit.placeholder_text = "prim1…"
	token_edit.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	jrow.add_child(token_edit)
	paste_button = Button.new()
	paste_button.text = "Paste"
	paste_button.pressed.connect(func(): token_edit.text = DisplayServer.clipboard_get().strip_edges())
	jrow.add_child(paste_button)
	join_button = Button.new()
	join_button.text = "Join"
	join_button.pressed.connect(func():
		if name_edit.text.strip_edges() != "":
			session.set_display_name(name_edit.text)
		session.join_room(token_edit.text))
	jrow.add_child(join_button)

	_pending_box = VBoxContainer.new()
	root.add_child(_pending_box)
	cancel_button = Button.new()
	cancel_button.text = "Cancel"
	cancel_button.pressed.connect(func(): session.cancel_start())
	_pending_box.add_child(cancel_button)

	_room_box = VBoxContainer.new()
	_room_box.add_theme_constant_override("separation", 8)
	root.add_child(_room_box)
	invite_button = Button.new()
	invite_button.text = "Invite a friend"
	invite_button.pressed.connect(func(): session.make_invite())
	_room_box.add_child(invite_button)
	var irow := HBoxContainer.new()
	_room_box.add_child(irow)
	invite_edit = LineEdit.new()
	invite_edit.editable = false
	invite_edit.placeholder_text = "one-time invite appears here"
	invite_edit.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	irow.add_child(invite_edit)
	copy_button = Button.new()
	copy_button.text = "Copy"
	copy_button.pressed.connect(func():
		DisplayServer.clipboard_set(invite_edit.text)
		copy_button.text = "Copied")
	irow.add_child(copy_button)
	roster = RichTextLabel.new()
	roster.bbcode_enabled = true
	roster.fit_content = true
	roster.scroll_active = false
	_room_box.add_child(roster)
	mic_button = CheckButton.new()
	mic_button.text = "Microphone on"
	mic_button.toggled.connect(func(on): if on != session.voice.get("mic", false): session.set_mic(on))
	_room_box.add_child(mic_button)
	deafen_button = CheckButton.new()
	deafen_button.text = "Deafen (hear no one)"
	deafen_button.toggled.connect(func(on): if on != session.voice.get("deaf", false): session.set_deafen(on))
	_room_box.add_child(deafen_button)
	leave_button = Button.new()
	leave_button.text = "Leave room"
	leave_button.pressed.connect(func(): session.leave_room())
	_room_box.add_child(leave_button)
	quit_button = Button.new()
	quit_button.text = "Quit Prim (ends the room session and voice)"
	quit_button.pressed.connect(func(): session.leave_and_quit())
	root.add_child(quit_button)
	session.changed.connect(refresh)
	refresh()


func refresh() -> void:
	var s = session
	var ph: String = s.phase
	title.text = "Prim"
	if s.me != "":
		var nm: String = s.display_name()
		identity.text = "You are %s  (id %s)" % [nm if nm != "" else "unnamed", s.short(s.me)]
	else:
		identity.text = "Your id appears once you are in a room."
	if not name_edit.has_focus() and s.display_name() != "" and name_edit.text != s.display_name():
		name_edit.text = s.display_name()
	error_label.text = s.error
	_choose_box.visible = ph == "choose" or ph == "removed"
	_pending_box.visible = ph == "pending"
	_room_box.visible = ph == "in_room"
	match ph:
		"connecting": status.text = "Looking for a running room…"
		"choose": status.text = "No room yet."
		"pending": status.text = "Joining… waiting for the room to answer. An invite that is used up, expired or revoked is refused; press Cancel to stop waiting." if s.starting_kind == "join" else "Starting the room…"
		"in_room": status.text = "In the room (%s)%s" % [s.lifecycle, "  · attached to a running daemon" if s.attached else ""]
		"removed": status.text = "You are not in a room."
	if invite_edit.text != s.invite:
		invite_edit.text = s.invite
		copy_button.text = "Copy"
	if mic_button.button_pressed != s.voice.get("mic", false):
		mic_button.set_pressed_no_signal(s.voice.get("mic", false))
	if deafen_button.button_pressed != s.voice.get("deaf", false):
		deafen_button.set_pressed_no_signal(s.voice.get("deaf", false))
	var vr: bool = s.voice.get("running", false)
	mic_button.disabled = not s.voice.get("wanted", false) and not vr
	deafen_button.disabled = mic_button.disabled
	invite_button.disabled = s.lifecycle != "open"
	copy_button.disabled = s.invite == ""
	var lines := PackedStringArray()
	lines.append("[b]In the room: %d[/b]" % s.members.size())
	var keys: Array = s.members.keys()
	keys.sort_custom(func(a, b): return (a == s.me) and (b != s.me))
	for k in keys:
		var nm2: String = s.names.get(k, "")
		var on: bool = s.members[k].get("online", false)
		lines.append("  %s%s  %s  [color=#888](%s)[/color]  %s" % [
			nm2 if nm2 != "" else "unnamed", " (you)" if k == s.me else "", "", s.short(k),
			"online" if on else "offline"])
	roster.text = "\n".join(lines)
