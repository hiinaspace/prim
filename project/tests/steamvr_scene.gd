extends SceneTree
var app
var output := OS.get_environment("PRIM_STEAMVR_TEST_OUTPUT")
var failures := []
func _initialize(): run.call_deferred()
func check(value: bool, message: String):
 print("CHECK ", message, ": ", value)
 if not value: failures.append(message)
func wait_for(predicate: Callable, seconds := 20.0) -> bool:
 var until := Time.get_ticks_msec() + int(seconds * 1000)
 while not predicate.call() and Time.get_ticks_msec() < until: await process_frame
 return predicate.call()
func run():
 if output.is_empty():
  push_error("Run through tools/test-steamvr-scene.py")
  quit(2)
  return
 app = load("res://main.tscn").instantiate()
 root.add_child(app)
 var ids := [app.session.get_instance_id(), app.player.get_instance_id(), app.sender.get_instance_id()]
 for cycle in range(2):
  app.xr_lifecycle.start()
  check(await wait_for(func():return app.xr_lifecycle.state == "xr"), "SteamVR OpenXR initialized")
  if app.xr_lifecycle.state != "xr": break
  var probe = load("res://tests/xr_render_probe.gd").new()
  var compositor := Compositor.new()
  compositor.compositor_effects = [probe]
  app.xr_camera.compositor = compositor
  check(await wait_for(func():return probe.snapshot().get("frame",0)>5), "SteamVR renders fresh frames")
  var sample: Dictionary = probe.snapshot()
  check(sample.get("buffers",0)==2 and sample.get("scene",0)==2, "SteamVR scene renders stereo")
  check(float(sample.get("ipd",0))>0.04, "SteamVR physical IPD")
  print("TRACKED ",app.sample_avatar_frame().tracked)
  if cycle == 0:
   FileAccess.open(output+"/prim-scene.ready",FileAccess.WRITE).store_string("ready")
   check(await wait_for(func():return app.xr_lifecycle.state == "desktop",30), "game evicts XR session without terminating Prim")
   var survive_until := Time.get_ticks_msec() + 45000
   await wait_for(func(): return Time.get_ticks_msec() >= survive_until, 50)
   check(app.xr_lifecycle.state == "desktop", "Prim remains alive 45 seconds after eviction")
   FileAccess.open(output+"/prim-scene.evicted",FileAccess.WRITE).store_string("alive")
   check(await wait_for(func():return FileAccess.file_exists(output+"/game.done"),30), "reference game finished")
  else:
   await app.xr_lifecycle.stop()
   check(app.xr_lifecycle.state=="desktop", "ordinary disable returns desktop")
  check(ids==[app.session.get_instance_id(),app.player.get_instance_id(),app.sender.get_instance_id()], "party media microphone objects preserved")
 print("STEAMVR_PRIM_RESULT ",JSON.stringify(failures))
 FileAccess.open(output+"/prim-scene.result",FileAccess.WRITE).store_string(JSON.stringify(failures))
 quit(0 if failures.is_empty() else 1)
