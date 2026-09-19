extends SceneTree
var life
func _initialize(): run.call_deferred()
func run():
 root.title = "Prim reference game"
 var origin := XROrigin3D.new()
 var camera := XRCamera3D.new()
 origin.add_child(camera)
 root.add_child(origin)
 camera.make_current()
 var environment := WorldEnvironment.new()
 environment.environment = Environment.new()
 environment.environment.background_mode = Environment.BG_COLOR
 environment.environment.background_color = Color("182a44")
 root.add_child(environment)
 for i in range(7):
  var cube := MeshInstance3D.new()
  cube.mesh = BoxMesh.new()
  cube.position = Vector3((i-3)*1.5, 1, -3)
  var material := StandardMaterial3D.new()
  material.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
  material.albedo_color = Color.from_hsv(float(i)/7, 0.7, 0.8)
  cube.material_override = material
  root.add_child(cube)
 var label := Label3D.new()
 label.text = "REFERENCE GAME\nLift to peek into Prim"
 label.position = Vector3(0,2.2,-3)
 label.font_size = 64
 root.add_child(label)
 life = preload("res://xr/session_lifecycle.gd").new()
 root.add_child(life)
 life.start()
 while life.state in ["desktop","starting"]: await process_frame
 if life.state != "xr": quit(1); return
 print("REFERENCE_READY")
 while not FileAccess.file_exists(OS.get_environment("PRIM_REFERENCE_STOP")): await process_frame
 await life.stop()
 quit()
