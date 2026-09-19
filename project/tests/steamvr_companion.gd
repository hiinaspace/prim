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
 var dimensions := root.get_visible_rect().size
 app.xr_lifecycle.start()
 check(await wait_for(func():return app.background_tracking and app.sample_avatar_frame().tracked==7),"game-first background head and hands")
 check(app.xr_lifecycle.state=="background" and not app.xr and not app.xr_lifecycle.interface.is_initialized(),"game-first does not create OpenXR scene")
 check(app.xr_lifecycle.observation.get("activity")=="other","game ownership detected")
 check(not app.laser.visible and not app.menu.headset_input_allowed,"background has no controller UI")
 check(app.audio_listener.get_parent()==app.desktop_camera,"physical head drives desktop spatial listener")
 await remote_check("background")
 var origin: Vector3=app.rig.global_position
 var local_head: Vector3=app.camera.position
 app.move_body(Vector3(0.1,0,0))
 check(app.rig.global_position.distance_to(origin)>0.05 and app.camera.position.is_equal_approx(local_head),"desktop movement translates playspace")
 var original_tracker: StringName=app.left.tracker
 var tracker:=XRControllerTracker.new()
 tracker.type=XRServer.TRACKER_CONTROLLER
 tracker.name=&"/test/steamvr-controller"
 tracker.set_pose(app.left.pose,Transform3D.IDENTITY,Vector3.ZERO,Vector3.ZERO,XRPose.XR_TRACKING_CONFIDENCE_HIGH)
 XRServer.add_tracker(tracker)
 app.left.tracker=tracker.name
 tracker.set_input("primary",Vector2(0,1))
 origin=app.rig.global_position
 for i in range(5): await process_frame
 check(app.rig.global_position.is_equal_approx(origin),"game controller stick never moves Prim")
 app.left.tracker=original_tracker
 XRServer.remove_tracker(tracker)
 app.menu.open_desktop()
 check(app.menu.desktop_surface.visible and not app.menu.quad.visible,"background desktop menu stays available")
 app.xr_lifecycle.request_open_room()
 check(app.menu.xr_takeover_dialog.visible,"deliberate takeover requests confirmation")
 app.menu.xr_takeover_dialog.hide()
 check(app.xr_lifecycle.state=="background","cancel leaves game running")
 await app.xr_lifecycle.stop()
 check(not app.background_tracking and not app.xr_lifecycle.enabled_intent and app.xr_lifecycle.bridge==null,"disable detaches background helper")
 app.xr_lifecycle.start()
 check(await wait_for(func():return app.background_tracking and app.sample_avatar_frame().tracked==7),"reenable alongside game")
 await stage("stop-game")
 check(await wait_for(func():return app.xr_lifecycle.state=="xr",25),"game exit automatically opens Prim")
 check(app.xr and not app.background_tracking and not app.xr_lifecycle.overlay_active,"ordinary scene presentation with OpenXR tracking")
 check(await wait_for(func():return app.sample_avatar_frame().tracked==7),"OpenXR head and hands after handoff")
 await remote_check("scene")
 if app.xr_lifecycle.bridge:
  var poses: Array=app.xr_lifecycle.bridge.snapshot().get("poses",[])
  if poses.size()==3:
   for i in range(2):
    var background: Transform3D=app.xr_lifecycle.OpenVRBridge.pose_transform(poses[i+1])
    var bg_head: Transform3D=app.xr_lifecycle.OpenVRBridge.pose_transform(poses[0])
    var bg_relative := bg_head.affine_inverse()*background
    var xr_relative: Transform3D=app.xr_camera.transform.affine_inverse()*app.grips[i].transform
    print("GRIP_COMPARE ",i," position=",bg_relative.origin.distance_to(xr_relative.origin)," angle=",bg_relative.basis.get_rotation_quaternion().angle_to(xr_relative.basis.get_rotation_quaternion()))
    print("XR_HEAD ",app.xr_camera.transform," BG_HEAD ",bg_head," XR_GRIP ",app.grips[i].transform," BG_GRIP ",background)
 await stage("start-game")
 check(await wait_for(func():return app.xr_lifecycle.state=="background" and app.sample_avatar_frame().tracked==7,25),"game launch returns to background tracking")
 check(app.xr_lifecycle.enabled_intent,"game eviction preserves Enable VR intent")
 await remote_check("evicted")
 check(root.get_visible_rect().size==dimensions,"desktop size survives scene handoff")
 check(ids==[app.session.get_instance_id(),app.player.get_instance_id(),app.sender.get_instance_id()],"room media microphone objects survive")
 # Accept the explicit takeover this time. This is the only forced scene claim.
 app.xr_lifecycle.request_open_room()
 check(app.menu.xr_takeover_dialog.visible,"takeover is confirmed every time")
 app.menu.xr_takeover_dialog.confirmed.emit()
 check(await wait_for(func():return app.xr_lifecycle.state=="xr",25),"confirmed takeover opens Prim scene")
 await app.xr_lifecycle.stop()
 check(app.xr_lifecycle.state=="desktop" and not app.xr_lifecycle.enabled_intent,"disable scene returns desktop")
 check(app.xr_lifecycle.bridge==null,"disable scene releases helper")
 check(root.get_visible_rect().size==dimensions,"desktop size restored")
 FileAccess.open(output+".json",FileAccess.WRITE).store_string(JSON.stringify({"checks":checks,"failures":failures}))
 print("STEAMVR_COMPANION_RESULT ",JSON.stringify(failures))
 quit(0 if failures.is_empty() else 1)
