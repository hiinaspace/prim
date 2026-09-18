## Test-only observation of the data actually used by the render thread.
extends CompositorEffect

var lock := Mutex.new()
var latest := {}
var frames := 0
var capture_color := false
var sample_shader := RID()
var sample_pipeline := RID()
var sample_buffer := RID()
var sample_rd: RenderingDevice

func _init() -> void:
	effect_callback_type = EFFECT_CALLBACK_TYPE_POST_TRANSPARENT

func _render_callback(_type: int, data: RenderData) -> void:
	var buffers: RenderSceneBuffersRD = data.get_render_scene_buffers()
	var scene := data.get_render_scene_data()
	if not buffers or not scene: return
	var sample := {"buffers":buffers.get_view_count(), "scene":scene.get_view_count()}
	if scene.get_view_count() == 2:
		sample["ipd"] = scene.get_view_eye_offset(0).distance_to(scene.get_view_eye_offset(1))
		sample["left_projection"] = scene.get_view_projection(0)
		sample["right_projection"] = scene.get_view_projection(1)
	lock.lock()
	var capture := capture_color
	capture_color = false
	lock.unlock()
	if capture:
		var values := sample_alphas(buffers)
		sample["alphas"] = values.slice(0, buffers.get_view_count())
		sample["brightness"] = values.slice(2, 2 + buffers.get_view_count())
	lock.lock()
	frames += 1
	sample["frame"] = frames
	if not sample.has("alphas") and latest.has("alphas"):
		sample["alphas"] = latest.alphas
		sample["brightness"] = latest.brightness
	latest = sample
	lock.unlock()

func snapshot() -> Dictionary:
	lock.lock()
	var result := latest.duplicate()
	lock.unlock()
	return result

func request_color_sample() -> void:
	lock.lock()
	capture_color = true
	latest.erase("alphas")
	lock.unlock()

func sample_alphas(buffers: RenderSceneBuffersRD) -> PackedFloat32Array:
	# Internal scene textures are storage images, not transfer-source textures.
	# Read one alpha per eye through a tiny compute buffer, not texture readback.
	sample_rd = RenderingServer.get_rendering_device()
	if not sample_shader.is_valid():
		var source := RDShaderSource.new()
		source.source_compute = """#version 450
layout(local_size_x=1) in;
layout(rgba16f, set=0, binding=0) readonly uniform image2D color_image;
layout(std430, set=0, binding=1) buffer Result { float alpha[]; } result;
layout(push_constant, std430) uniform Params { int eye; int p1; int p2; int p3; } params;
void main() {
    vec4 color = imageLoad(color_image, ivec2(0));
    result.alpha[params.eye] = color.a;
    result.alpha[params.eye + 2] = color.r + color.g + color.b;
}
"""
		sample_shader = sample_rd.shader_create_from_spirv(sample_rd.shader_compile_spirv_from_source(source))
		sample_pipeline = sample_rd.compute_pipeline_create(sample_shader)
		sample_buffer = sample_rd.storage_buffer_create(16)
	for view in range(buffers.get_view_count()):
		var color := RDUniform.new()
		color.uniform_type = RenderingDevice.UNIFORM_TYPE_IMAGE
		color.binding = 0
		color.add_id(buffers.get_color_layer(view))
		var dest := RDUniform.new()
		dest.uniform_type = RenderingDevice.UNIFORM_TYPE_STORAGE_BUFFER
		dest.binding = 1
		dest.add_id(sample_buffer)
		var uniforms := UniformSetCacheRD.get_cache(sample_shader, 0, [color, dest])
		var list := sample_rd.compute_list_begin()
		sample_rd.compute_list_bind_compute_pipeline(list, sample_pipeline)
		sample_rd.compute_list_bind_uniform_set(list, uniforms, 0)
		sample_rd.compute_list_set_push_constant(list, PackedInt32Array([view, 0, 0, 0]).to_byte_array(), 16)
		sample_rd.compute_list_dispatch(list, 1, 1, 1)
		sample_rd.compute_list_end()
	var values := sample_rd.buffer_get_data(sample_buffer).to_float32_array()
	return values

func _notification(what: int) -> void:
	if what == NOTIFICATION_PREDELETE and sample_shader.is_valid():
		sample_rd.free_rid(sample_buffer)
		sample_rd.free_rid(sample_shader)
