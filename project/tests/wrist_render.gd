extends SceneTree
func _initialize(): run.call_deferred()
func run():
 root.size = Vector2i(1000, 650)
 var camera := Camera3D.new()
 root.add_child(camera)
 camera.position = Vector3(0, 1.6, 0)
 camera.fov = 60
 var environment := WorldEnvironment.new()
 environment.environment = Environment.new()
 environment.environment.background_mode = Environment.BG_COLOR
 environment.environment.background_color = Color("243340")
 root.add_child(environment)
 var watch = preload("res://xr/wrist_controls.gd").new()
 root.add_child(watch)
 var hands := [Transform3D(Basis(Vector3.FORWARD, 1.0), Vector3(-0.13, 1.53, -0.45)), Transform3D(Basis(Vector3.RIGHT, 1.2), Vector3(0.13,1.53,-0.45))]
 watch.update_view(0.1,true,camera.global_transform,hands,[true,true],-1,false,true)
 for i in range(12): await process_frame
 await RenderingServer.frame_post_draw
 var image := root.get_texture().get_image()
 image.save_png(OS.get_environment("PRIM_WRIST_IMAGE"))
 quit()
