class_name PrimAvatarCatalog
extends RefCounted

const REVISION := 1
const MODELS := {
	"alicia": {"name":"Alicia Solid", "scene":preload("res://avatars/models/alicia.vrm")},
	"vita": {"name":"Vita", "scene":preload("res://avatars/models/vita.vrm")},
}
const DEFAULT := "alicia"

static func integer_in_range(value: Variant, high: int) -> bool:
	return (value is int or value is float) and is_finite(float(value)) and value >= 0 and value <= high and value == floor(float(value))

static func valid_state(state: Dictionary) -> bool:
	return integer_in_range(state.get("revision"), 0xffff) and state.get("avatar") is String \
		and (state.get("eye_height") is float or state.get("eye_height") is int) \
		and is_finite(state.eye_height) and state.eye_height >= 0.5 and state.eye_height <= 2.5 \
		and integer_in_range(state.get("epoch"), 0xffffffff)
