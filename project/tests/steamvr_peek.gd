extends SceneTree
var app
var failures := []
var checks := 0
var output := OS.get_environment("PRIM_TEST_OUTPUT")
var role := OS.get_environment("PRIM_TEST_ROLE")
var remote_pose := {}
var replies := {}
func _initialize(): run.call_deferred()
func check(ok: bool, label: String):
 checks += 1
 print("CHECK ", label, ": ", ok)
 if not ok: failures.append(label)
func wait_for(predicate: Callable, seconds := 15.0) -> bool:
 var until := Time.get_ticks_msec() + int(seconds * 1000)
 while not predicate.call() and Time.get_ticks_msec() < until: await process_frame
 return predicate.call()
func stage(name: String):
 FileAccess.open(output+".stage",FileAccess.WRITE).store_string(name)
 check(await wait_for(func():return FileAccess.file_exists(output+"."+name)),"runner "+name)
func remote_check(phase: String):
 var until := Time.get_ticks_msec() + 5000
 while Time.get_ticks_msec() < until and int(replies.get(phase,{}).get("tracked",0)) != 7:
  app.session.broadcast_control(JSON.stringify({"type":"steamvr_probe","phase":phase}))
  var next := Time.get_ticks_msec() + 100
  await wait_for(func():return Time.get_ticks_msec() >= next, 1)
 check(replies.has(phase),"observer replies "+phase)
 check(int(replies.get(phase,{}).get("tracked",0))==7,"remote tracked head and hands "+phase)
func run():
 if output.is_empty(): quit(2); return
 app = load("res://main.tscn").instantiate()
 root.add_child(app)
 root.gui_embed_subwindows = true
 app.session.pose_received.connect(func(_peer,bytes):remote_pose=app.Pose.decode(bytes))
 app.session.message_received.connect(func(peer,raw):
  var message = JSON.parse_string(raw)
  if not message is Dictionary: return
  if message.get("type")=="steamvr_probe":
   app.session.send_control(peer,JSON.stringify({"type":"steamvr_reply","phase":message.phase,"tracked":remote_pose.get("tracked",0)}))
  elif message.get("type")=="steamvr_reply": replies[message.phase]=message)
 if role=="observer":
  app.toggle_connection()
  await wait_for(func():return FileAccess.file_exists(output+".stop"),180)
  quit(0)
  return
 app.session.endpoint_ready.connect(func():FileAccess.open(output+".endpoint",FileAccess.WRITE).store_string(app.session.get_endpoint_info()))
 app.toggle_connection()
 check(await wait_for(func():return FileAccess.file_exists(output+".endpoint")),"room endpoint ready")
 await stage("observer")
 check(await wait_for(func():return app.avatars.size()==1),"second client connected")
 var ids := [app.session.get_instance_id(),app.player.get_instance_id(),app.sender.get_instance_id()]
 app.xr_lifecycle.start()
 check(await wait_for(func():return app.xr_lifecycle.openvr_active,25),"game-first starts OpenVR overlay without scene takeover")
 check(await wait_for(func():return app.sample_avatar_frame().tracked==7,25),"OpenVR tracks head and both grip poses")
 await remote_check("openvr-hidden")
 check(not app.menu.headset_input_allowed,"game retains ordinary controller UI")
 if OS.get_environment("PRIM_PEEK_INTERACTIVE") == "1":
  print("INTERACTIVE_PEEK_READY: lift gesture and wrist controls available for 2 minutes")
  await create_timer(120.0).timeout
 for cycle in range(3):
  app.xr_lifecycle.show_reveal()
  check(await wait_for(func():return app.xr_lifecycle.presentation_fraction>0.9),"explicit OpenVR reveal")
  await create_timer(1.0).timeout
  app.xr_lifecycle.hide_reveal()
  check(await wait_for(func():return app.xr_lifecycle.presentation_fraction==0),"OpenVR hide")
 check(app.xr_lifecycle.observation.get("activity")=="other","reference game remains scene owner")
 await stage("stop-game")
 check(await wait_for(func():return app.xr_lifecycle.state=="xr" and not app.xr_lifecycle.openvr_active,25),"game exit hands overlay back to OpenXR scene")
 check(await wait_for(func():return app.sample_avatar_frame().tracked==7,15),"OpenXR tracking after overlay detach")
 await stage("start-game")
 check(await wait_for(func():return app.xr_lifecycle.openvr_active,25),"new game hands OpenXR back to overlay")
 check(await wait_for(func():return app.sample_avatar_frame().tracked==7,15),"OpenVR tracking after second handoff")
 await remote_check("openvr-return")
 check(ids==[app.session.get_instance_id(),app.player.get_instance_id(),app.sender.get_instance_id()],"voice media and room survive provider switches")
 await app.xr_lifecycle.stop()
 check(app.xr_lifecycle.state=="desktop" and not app.xr_lifecycle.openvr_active,"disable closes overlay without closing Prim")
 FileAccess.open(output+".json",FileAccess.WRITE).store_string(JSON.stringify({"checks":checks,"failures":failures}))
 quit(0 if failures.is_empty() else 1)
