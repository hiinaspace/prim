## X0: full-resolution crop of each eye's colour buffer (no downsampling), with a
## monotonic render-callback timestamp. Same buffer the headset gets, like eye_probe.gd.
## request(rect) with rect in 0..1 of the eye buffer; poll take().
extends CompositorEffect

var _lock := Mutex.new()
var _req := {}
var _res := {}
var _frames := 0
var _rd: RenderingDevice
var _shader := RID()
var _pipeline := RID()
var _sampler := RID()
var _buf := {} # bytes -> [RID per view]
var _error := ""

const SHADER := """#version 450
layout(local_size_x = 8, local_size_y = 8) in;
layout(set = 0, binding = 0) uniform sampler2D color_tex;
layout(std430, set = 0, binding = 1) buffer Out { float v[]; } outp;
layout(push_constant, std430) uniform P { ivec2 org; ivec2 dim; ivec2 size; ivec2 pad; } p;
void main() {
	ivec2 c = ivec2(gl_GlobalInvocationID.xy);
	if (c.x >= p.dim.x || c.y >= p.dim.y) return;
	ivec2 px = clamp(p.org + c, ivec2(0), p.size - 1);
	vec4 s = texelFetch(color_tex, px, 0);
	int o = (c.y * p.dim.x + c.x) * 4;
	vec3 g = pow(clamp(s.rgb, 0.0, 1.0), vec3(1.0 / 2.2)); // display-referred so 8-bit PNGs keep the dark surround
	outp.v[o] = g.r; outp.v[o + 1] = g.g; outp.v[o + 2] = g.b; outp.v[o + 3] = 1.0;
}
"""

func _init() -> void:
	effect_callback_type = EFFECT_CALLBACK_TYPE_POST_TRANSPARENT

func request(rect: Rect2, tag: String) -> void:
	_lock.lock()
	_req = {"rect": rect, "tag": tag}
	_res = {}
	_lock.unlock()

func take() -> Dictionary:
	_lock.lock()
	var r := _res
	_res = {}
	_lock.unlock()
	return r

func frames() -> int:
	return _frames

func _render_callback(_t: int, data: RenderData) -> void:
	var buffers: RenderSceneBuffersRD = data.get_render_scene_buffers()
	if not buffers: return
	_lock.lock()
	_frames += 1
	var req := _req
	_req = {}
	var n := _frames
	_lock.unlock()
	if req.is_empty(): return
	var t_us := Time.get_ticks_usec()
	var size: Vector2i = buffers.get_internal_size()
	var rect: Rect2 = req.rect
	var org := Vector2i(int(rect.position.x * size.x), int(rect.position.y * size.y))
	var dim := Vector2i(int(rect.size.x * size.x), int(rect.size.y * size.y))
	var eyes := _sample(buffers, org, dim, size)
	var t2 := Time.get_ticks_usec()
	_lock.lock()
	_res = {"tag": req.tag, "frame": n, "t_us": t_us, "t_done_us": t2, "size": size, "org": org, "dim": dim, "eyes": eyes, "error": _error}
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
	_sampler = _rd.sampler_create(RDSamplerState.new())
	return true

func _sample(buffers: RenderSceneBuffersRD, org: Vector2i, dim: Vector2i, size: Vector2i) -> Array:
	if not _shader.is_valid() and not _setup(): return []
	var count := buffers.get_view_count()
	var bytes := dim.x * dim.y * 16
	if not _buf.has(bytes):
		_buf[bytes] = []
	var bl: Array = _buf[bytes]
	while bl.size() < count: bl.append(_rd.storage_buffer_create(bytes))
	for view in count:
		var color := RDUniform.new()
		color.uniform_type = RenderingDevice.UNIFORM_TYPE_SAMPLER_WITH_TEXTURE
		color.binding = 0
		color.add_id(_sampler)
		color.add_id(buffers.get_color_layer(view))
		var dest := RDUniform.new()
		dest.uniform_type = RenderingDevice.UNIFORM_TYPE_STORAGE_BUFFER
		dest.binding = 1
		dest.add_id(bl[view])
		var set := UniformSetCacheRD.get_cache(_shader, 0, [color, dest])
		var list := _rd.compute_list_begin()
		_rd.compute_list_bind_compute_pipeline(list, _pipeline)
		_rd.compute_list_bind_uniform_set(list, set, 0)
		var pc := PackedInt32Array([org.x, org.y, dim.x, dim.y, size.x, size.y, 0, 0]).to_byte_array()
		_rd.compute_list_set_push_constant(list, pc, pc.size())
		_rd.compute_list_dispatch(list, (dim.x + 7) / 8, (dim.y + 7) / 8, 1)
		_rd.compute_list_end()
	var eyes := []
	for view in count:
		eyes.append(_rd.buffer_get_data(bl[view]).to_float32_array())
	return eyes
