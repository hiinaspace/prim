class_name PrimAvatarDriver
extends Node3D

const Expressions = preload("res://avatars/expressions.gd")
var expressions: PrimAvatarExpressions

const Catalog = preload("res://avatars/catalog.gd")
const Spine = preload("res://addons/renik/renik_spine.gd")
const Limb = preload("res://addons/renik/renik_limb.gd")
const Placement = preload("res://addons/renik/renik_placement.gd")
const Fingers = preload("res://avatars/fingers.gd")
const LOCAL_FIRST := 1 << 17
const LOCAL_THIRD := 1 << 18
const REMOTE := 1 << 19
var model: Node3D
var skeleton: Skeleton3D
var head_target: Node3D
var hips_target: Node3D
var hand_targets: Array[Node3D] = []
var foot_targets: Array[Node3D] = []
var placement
var fingers
var spring_nodes: Array[Node] = []
var eye_height := 1.6
var model_scale := 1.0
var view_offset := Vector3(0, 0.023, -0.015)
var reset_pending := false
var ready_pose := false
var last_view := Transform3D.IDENTITY
var avatar_id := ""

func configure(id: String, height: float, local_body: bool) -> bool:
	if not Catalog.MODELS.has(id): return false
	avatar_id = id
	eye_height = height
	model = Catalog.MODELS[id].scene.instantiate()
	skeleton = find_skeleton(model)
	if not skeleton:
		model.free()
		return false
	# The normalized VRM humanoid faces +Z. World tracking faces -Z.
	model.rotation.y = PI
	var head_rest := skeleton.get_bone_global_rest(skeleton.find_bone("Head"))
	model_scale = height / (head_rest.origin.y + view_offset.y)
	model.scale = Vector3.ONE * model_scale
	prepare_meshes(model, local_body)
	add_child(model)
	expressions = Expressions.new()
	expressions.configure(model)
	head_target = marker("HeadTarget", head_rest)
	hips_target = marker("HipsTarget", skeleton.get_bone_global_rest(skeleton.find_bone("Hips")))
	for side in ["Left", "Right"]:
		hand_targets.append(marker(side + "HandTarget", skeleton.get_bone_global_rest(skeleton.find_bone(side + "Hand"))))
		foot_targets.append(marker(side + "FootTarget", skeleton.get_bone_global_rest(skeleton.find_bone(side + "Foot"))))
	var spine := Spine.new()
	spine.head_target = head_target
	spine.hip_target = hips_target
	skeleton.add_child(spine)
	for i in range(2):
		add_limb(i, false)
		add_limb(i, true)
	fingers = Fingers.new()
	skeleton.add_child(fingers)
	placement = Placement.new()
	placement.armature_skeleton_path = NodePath("..")
	placement.armature_head_target = NodePath("../HeadTarget")
	placement.armature_hip_target = NodePath("../HipsTarget")
	placement.armature_left_foot_target = NodePath("../LeftFootTarget")
	placement.armature_right_foot_target = NodePath("../RightFootTarget")
	placement.enable_hip_placement = true
	placement.collision_mask = 2
	skeleton.add_child(placement)
	# Driver schedules placement explicitly. Its built-in interpolation stays off
	# so only legs/hips lag physics; head and wrists use this render frame's pose.
	placement.set_process_internal(false)
	placement.set_physics_process_internal(false)
	return true

static func find_skeleton(node: Node) -> Skeleton3D:
	if node is Skeleton3D and node.find_bone("Hips") >= 0: return node
	for child in node.get_children():
		var found := find_skeleton(child)
		if found: return found
	return null

func prepare_meshes(node: Node, local_body: bool) -> void:
	if node is VRMSecondary: spring_nodes.append(node)
	if node is MeshInstance3D:
		var first: bool = node.layers & 2 != 0
		var third: bool = node.layers & 4 != 0
		node.layers = ((LOCAL_FIRST if first else 0) | (LOCAL_THIRD if third else 0)) if local_body else (REMOTE if third else 0)
		# Only the complete representation casts shadows; shared meshes still cast once.
		if first and not third: node.cast_shadow = GeometryInstance3D.SHADOW_CASTING_SETTING_OFF
	for child in node.get_children(): prepare_meshes(child, local_body)

func marker(label: String, rest: Transform3D) -> Node3D:
	var target := Node3D.new()
	target.name = label
	skeleton.add_child(target)
	target.transform = rest
	return target

func add_limb(index: int, leg: bool) -> void:
	var side := "Left" if index == 0 else "Right"
	var limb := Limb.new()
	limb.preset = index + (2 if leg else 0)
	limb.leaf_bone = side + ("Foot" if leg else "Hand")
	limb.lower_bone = side + ("LowerLeg" if leg else "LowerArm")
	limb.upper_bone = side + ("UpperLeg" if leg else "UpperArm")
	limb.mirror = index == 1
	limb.target = foot_targets[index] if leg else hand_targets[index]
	if leg:
		limb.assign_leg_defaults.call()
		limb.has_shoulder = false
	else:
		limb.dynamic_pole_root_bone = "Hips"
		limb.dynamic_pole_head_bone = "Head"
	skeleton.add_child(limb)

func apply_frame(poses: Array[Transform3D], tracked: int, rotations: Array[Quaternion], masks: PackedInt32Array, fallback_curls: PackedFloat32Array, reset: bool = false) -> void:
	if not skeleton or poses.size() != 3: return
	if not ready_pose or last_view.origin.distance_to(poses[0].origin) > 1.0: reset = true
	last_view = poses[0]
	# Root translation follows the view horizontally. The skeleton itself solves yaw.
	global_position = Vector3(poses[0].origin.x, 0, poses[0].origin.z)
	var head_basis := poses[0].basis * Basis(Vector3.UP, PI)
	head_target.global_transform = Transform3D(head_basis.scaled(Vector3.ONE * model_scale), poses[0].origin - head_basis * (view_offset * model_scale))
	for i in range(2):
		var target := poses[i + 1]
		if tracked & (1 << i) == 0:
			var forward := -poses[0].basis.z
			forward.y = 0
			if forward.length_squared() < 0.01: forward = Vector3.FORWARD
			var yaw := Basis.looking_at(forward.normalized())
			var x := -0.22 if i == 0 else 0.22
			target = Transform3D(yaw * Basis(Vector3.FORWARD, PI / 2 if i == 0 else -PI / 2), Vector3(poses[0].origin.x, eye_height * 0.43, poses[0].origin.z) + yaw * Vector3(x * eye_height / 1.6, 0, -0.04))
		hand_targets[i].global_transform = target
	fingers.rotations = rotations
	fingers.masks = masks
	fingers.curls = fallback_curls
	ready_pose = true
	if reset: reset_pending = true

func _physics_process(delta: float) -> void:
	if ready_pose and is_visible_in_tree():
		if reset_pending:
			placement.foot_place(0.016, head_target.global_transform, get_world_3d(), true)
			placement.hip_place(0.016, head_target.global_transform, placement.target_left_foot, placement.target_right_foot, 0.0, true)
			placement.target_foot_is_valid = true
			placement.target_hip_is_valid = true
			reset_springs()
			reset_pending = false
		else:
			placement.update_placement(minf(delta, 0.05))
		placement.interpolate_transforms(1.0)

func reset_springs() -> void:
	for secondary in spring_nodes:
		secondary.update_centers(skeleton.global_transform)
		for i in range(secondary.spring_bones_internal.size()):
			var center: int = secondary.springs_centers[i]
			secondary.spring_bones_internal[i].setup(secondary.center_transforms_inv[center], true)

func apply_visemes(weights: PackedFloat32Array, delta: float) -> void:
	if expressions: expressions.apply(weights, delta)

func reset_visemes() -> void:
	if expressions: expressions.reset()
