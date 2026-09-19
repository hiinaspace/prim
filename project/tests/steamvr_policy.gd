extends SceneTree
const Bridge = preload("res://xr/openvr_bridge.gd")
var failures := []
var checks := 0
func check(ok: bool, label: String):
 checks += 1
 print("CHECK ",label,": ",ok)
 if not ok: failures.append(label)
func _initialize():
 var sample := {"age_ms":5,"scene_pid":0,"scene_state":0,"starting":"","starting_error":0,"key_error":0,"key":""}
 check(Bridge.classify(sample,123)=="idle","empty stable scene is idle")
 sample.scene_pid=321
 sample.scene_state=3
 sample.key=Bridge.HOME_KEY
 check(Bridge.classify(sample,123)=="idle","Home can yield to Prim")
 sample.key="steam.app.game"
 check(Bridge.classify(sample,123)=="other","game is not idle")
 sample.scene_state=4
 check(Bridge.classify(sample,123)=="other","game not drawing remains occupied")
 sample.scene_state=3
 check(Bridge.classify(sample,123)=="other","dashboard does not change owner")
 sample.key_error=100
 check(Bridge.classify(sample,123)=="other","unknown live process remains occupied")
 sample.scene_pid=123
 check(Bridge.classify(sample,123)=="self","own scene excluded from games")
 sample.scene_state=2
 check(Bridge.classify(sample,123)=="transition","quitting own scene is a transition")
 sample.scene_state=1
 check(Bridge.classify(sample,123)=="transition","starting scene is a transition")
 sample.scene_state=3
 sample.starting="steam.app.next"
 check(Bridge.classify(sample,123)=="transition","pending game prevents claim")
 sample.starting=""
 sample.scene_pid=0
 check(Bridge.classify(sample,123)=="unknown","no PID with active state is not idle")
 sample.scene_state=0
 sample.age_ms=501
 check(Bridge.classify(sample,123)=="unknown","stale idle does not allow claim")
 sample.age_ms=0
 sample.starting_error=101
 check(Bridge.classify(sample,123)=="unknown","failed transition query is not idle")
 sample.starting_error=102
 check(Bridge.classify(sample,123)=="idle","no application query allows idle")
 var pose := {"valid":true,"matrix":[0,0,1,3,0,1,0,4,-1,0,0,5]}
 sample.poses=[pose,pose,pose]
 check(Bridge.valid_snapshot(sample),"bounded pose packet accepted")
 var t: Transform3D=Bridge.pose_transform(pose)
 check(t.origin.is_equal_approx(Vector3(3,4,5)),"standing-space translation preserves metres")
 check((t.basis*Vector3.RIGHT).is_equal_approx(Vector3(0,0,-1)),"row-major matrix maps Godot basis columns")
 sample.poses=[pose]
 check(not Bridge.valid_snapshot(sample),"partial packet rejected")
 pose.matrix[0]=NAN
 sample.poses=[pose,pose,pose]
 check(not Bridge.valid_snapshot(sample),"nonfinite pose rejected")
 print("STEAMVR_POLICY_RESULT ",JSON.stringify({"checks":checks,"failures":failures}))
 quit(0 if failures.is_empty() else 1)
