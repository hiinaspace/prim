## Changes only the headset projection's alpha; desktop rendering is independent.
extends CompositorEffect

var rd: RenderingDevice
var shader := RID()
var pipeline := RID()
var amount := 1.0
var mutex := Mutex.new()
const SOURCE := """#version 450
layout(local_size_x=8, local_size_y=8, local_size_z=1) in;
layout(rgba16f, set=0, binding=0) uniform image2D color_image;
layout(push_constant, std430) uniform Params { float amount; float pad0; float pad1; float pad2; } params;
void main() {
    ivec2 p = ivec2(gl_GlobalInvocationID.xy);
    if (any(greaterThanEqual(p, imageSize(color_image)))) return;
    vec4 color = imageLoad(color_image, p);
    imageStore(color_image, p, vec4(color.rgb, params.amount));
}
"""

func _init() -> void:
	effect_callback_type = EFFECT_CALLBACK_TYPE_POST_TRANSPARENT
	access_resolved_color = true

func set_amount(value: float) -> void:
	mutex.lock()
	amount = clampf(value, 0.0, 1.0)
	mutex.unlock()

func _render_callback(_type: int, data: RenderData) -> void:
	if not rd: rd = RenderingServer.get_rendering_device()
	if not rd: return
	if not shader.is_valid():
		var source := RDShaderSource.new()
		source.source_compute = SOURCE
		var spirv := rd.shader_compile_spirv_from_source(source)
		if not spirv.compile_error_compute.is_empty():
			push_error(spirv.compile_error_compute)
			return
		shader = rd.shader_create_from_spirv(spirv)
		pipeline = rd.compute_pipeline_create(shader)
	var buffers := data.get_render_scene_buffers() as RenderSceneBuffersRD
	if not buffers: return
	var size := buffers.get_internal_size()
	if size.x == 0 or size.y == 0: return
	mutex.lock()
	var value := amount
	mutex.unlock()
	var params := PackedFloat32Array([value, 0, 0, 0]).to_byte_array()
	for view in range(buffers.get_view_count()):
		var uniform := RDUniform.new()
		uniform.uniform_type = RenderingDevice.UNIFORM_TYPE_IMAGE
		uniform.binding = 0
		uniform.add_id(buffers.get_color_layer(view))
		var uniform_set := UniformSetCacheRD.get_cache(shader, 0, [uniform])
		var list := rd.compute_list_begin()
		rd.compute_list_bind_compute_pipeline(list, pipeline)
		rd.compute_list_bind_uniform_set(list, uniform_set, 0)
		rd.compute_list_set_push_constant(list, params, params.size())
		rd.compute_list_dispatch(list, ceili(size.x / 8.0), ceili(size.y / 8.0), 1)
		rd.compute_list_end()

func _notification(what: int) -> void:
	if what == NOTIFICATION_PREDELETE and shader.is_valid():
		# RenderingServer owns this device and queues frees safely.
		rd.free_rid(shader)
