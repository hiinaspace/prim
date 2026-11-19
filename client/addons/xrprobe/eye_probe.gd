## Reads what each eye's render buffer actually holds (not the desktop window).
## On request, a compute pass downsamples every view's color and depth layer to
## a THUMB x THUMB grid: linear RGBA plus the fraction of each cell whose depth
## is still the clear value ("background": sky or clear color, never geometry).
## Seeded from Prim's project/tests/xr_render_probe.gd.
extends CompositorEffect

const THUMB := 64
const SUB := 4 # SUB x SUB samples per thumbnail cell
const FLOATS_PER_CELL := 5

var _lock := Mutex.new()
var _frames := 0
var _views := {}
var _request := ""
var _result := {}
var _rd: RenderingDevice
var _shader := RID()
var _pipeline := RID()
var _sampler := RID()
var _buffers: Array[RID] = []
var _error := ""

const SHADER := """#version 450
layout(local_size_x = 8, local_size_y = 8) in;
layout(set = 0, binding = 0) uniform sampler2D color_tex;
layout(set = 0, binding = 1) uniform sampler2D depth_tex;
layout(std430, set = 0, binding = 2) buffer Out { float v[]; } outp;
layout(push_constant, std430) uniform P { ivec2 size; int thumb; int sub; } p;
void main() {
	ivec2 cell = ivec2(gl_GlobalInvocationID.xy);
	if (cell.x >= p.thumb || cell.y >= p.thumb) return;
	vec4 acc = vec4(0.0);
	float bg = 0.0;
	for (int j = 0; j < p.sub; j++) for (int i = 0; i < p.sub; i++) {
		vec2 uv = (vec2(cell * p.sub + ivec2(i, j)) + 0.5) / float(p.thumb * p.sub);
		ivec2 px = clamp(ivec2(uv * vec2(p.size)), ivec2(0), p.size - 1);
		acc += texelFetch(color_tex, px, 0);
		// Godot 4.3+ uses reverse Z: cleared depth is 0.0.
		bg += texelFetch(depth_tex, px, 0).r <= 0.0 ? 1.0 : 0.0;
	}
	float n = float(p.sub * p.sub);
	int o = (cell.y * p.thumb + cell.x) * 5;
	acc /= n;
	outp.v[o] = acc.r; outp.v[o + 1] = acc.g; outp.v[o + 2] = acc.b; outp.v[o + 3] = acc.a;
	outp.v[o + 4] = bg / n;
}
"""


func _init() -> void:
	effect_callback_type = EFFECT_CALLBACK_TYPE_POST_TRANSPARENT


## Ask for a capture on the next rendered frame; poll take_capture().
func request_capture(tag: String) -> void:
	_lock.lock()
	_request = tag
	_result = {}
	_lock.unlock()


## Returns {} until the requested capture is done, then
## {tag, frame, buffers, scene, size, eyes: [{rgba: PackedFloat32Array, ...}]}.
func take_capture() -> Dictionary:
	_lock.lock()
	var r := _result
	if not r.is_empty():
		_result = {}
	_lock.unlock()
	return r


## Every-frame counters, no GPU readback: {frame, buffers, scene, ipd, error}.
func views() -> Dictionary:
	_lock.lock()
	var r := _views.duplicate()
	_lock.unlock()
	return r


func _render_callback(_type: int, data: RenderData) -> void:
	var buffers: RenderSceneBuffersRD = data.get_render_scene_buffers()
	var scene := data.get_render_scene_data()
	if not buffers or not scene:
		return
	var v := {"buffers": buffers.get_view_count(), "scene": scene.get_view_count()}
	if scene.get_view_count() == 2:
		v["ipd"] = scene.get_view_eye_offset(0).distance_to(scene.get_view_eye_offset(1))
	_lock.lock()
	_frames += 1
	v["frame"] = _frames
	v["error"] = _error
	_views = v
	var tag := _request
	_request = ""
	_lock.unlock()
	if tag.is_empty():
		return
	var r := v.duplicate()
	r["tag"] = tag
	r["size"] = buffers.get_internal_size()
	r["eyes"] = _sample(buffers)
	_lock.lock()
	r["error"] = _error
	_result = r
	_lock.unlock()


func _setup() -> bool:
	_rd = RenderingServer.get_rendering_device()
	var src := RDShaderSource.new()
	src.source_compute = SHADER
	var spirv := _rd.shader_compile_spirv_from_source(src)
	if not spirv.compile_error_compute.is_empty():
		_error = "shader: " + spirv.compile_error_compute
		return false
	_shader = _rd.shader_create_from_spirv(spirv)
	_pipeline = _rd.compute_pipeline_create(_shader)
	var ss := RDSamplerState.new()
	_sampler = _rd.sampler_create(ss)
	return true


func _sample(buffers: RenderSceneBuffersRD) -> Array:
	if not _shader.is_valid() and not _setup():
		return []
	var size: Vector2i = buffers.get_internal_size()
	var count := buffers.get_view_count()
	var bytes := THUMB * THUMB * FLOATS_PER_CELL * 4
	while _buffers.size() < count:
		_buffers.append(_rd.storage_buffer_create(bytes))
	for view in count:
		var color := RDUniform.new()
		color.uniform_type = RenderingDevice.UNIFORM_TYPE_SAMPLER_WITH_TEXTURE
		color.binding = 0
		color.add_id(_sampler)
		color.add_id(buffers.get_color_layer(view))
		var depth := RDUniform.new()
		depth.uniform_type = RenderingDevice.UNIFORM_TYPE_SAMPLER_WITH_TEXTURE
		depth.binding = 1
		depth.add_id(_sampler)
		depth.add_id(buffers.get_depth_layer(view))
		var dest := RDUniform.new()
		dest.uniform_type = RenderingDevice.UNIFORM_TYPE_STORAGE_BUFFER
		dest.binding = 2
		dest.add_id(_buffers[view])
		var set := UniformSetCacheRD.get_cache(_shader, 0, [color, depth, dest])
		var list := _rd.compute_list_begin()
		_rd.compute_list_bind_compute_pipeline(list, _pipeline)
		_rd.compute_list_bind_uniform_set(list, set, 0)
		var pc := PackedInt32Array([size.x, size.y, THUMB, SUB]).to_byte_array()
		_rd.compute_list_set_push_constant(list, pc, pc.size())
		_rd.compute_list_dispatch(list, THUMB / 8, THUMB / 8, 1)
		_rd.compute_list_end()
	var eyes := []
	for view in count:
		eyes.append({"cells": _rd.buffer_get_data(_buffers[view]).to_float32_array()})
	return eyes


func _notification(what: int) -> void:
	if what == NOTIFICATION_PREDELETE and _rd:
		for b in _buffers:
			_rd.free_rid(b)
		if _sampler.is_valid():
			_rd.free_rid(_sampler)
		if _shader.is_valid():
			_rd.free_rid(_shader)
