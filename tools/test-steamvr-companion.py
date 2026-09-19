#!/usr/bin/env python3
"""Live Linux SteamVR companion/peer test; replaces the current headset scene."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import shutil
import time
ROOT=Path(__file__).resolve().parents[1]
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--allow-scene-takeover',action='store_true',required=True)
parser.add_argument('--peek', action='store_true', help='Exercise experimental OpenVR projective overlay handoffs')
a=parser.parse_args()
out=ROOT/'.local/steamvr-companion'/time.strftime('%Y%m%d-%H%M%S')
out.mkdir(parents=True)
print('Artifacts:',out,flush=True)
env=os.environ|{'DISPLAY':os.environ.get('DISPLAY',':0'),'WAYLAND_DISPLAY':os.environ.get('WAYLAND_DISPLAY','wayland-1'),
 'VR_OVERRIDE':str(Path.home()/'.local/share/Steam/steamapps/common/SteamVR'),
 'XR_RUNTIME_JSON':str(Path.home()/'.local/share/Steam/steamapps/common/SteamVR/steamxr_linux64.json'),
 'PRIM_NETWORK_LOCAL_ONLY':'1','LIBMPV_ZERO_MPV_LIBRARY':str(ROOT/'.local/mpv/lib/libmpv.so.2')}
for key in ('PRIM_AUTOJOIN','PRIM_MEDIA','PRIM_RUNTIME_VRM','PRIM_XR_SCENE'):env.pop(key,None)
if a.peek: env['PRIM_OPENVR_PEEK']='1'
logs=[]; processes=[]; game=None

def launch(command,environment,name):
 log=(out/(name+'.log')).open('w');logs.append(log)
 p=subprocess.Popen(['steam-run']+list(map(str,command)),env=environment,stdout=log,stderr=subprocess.STDOUT)
 processes.append(p);return p

def stop(p):
 if p and p.poll() is None:
  p.terminate()
  try:p.wait(timeout=10)
  except subprocess.TimeoutExpired:p.kill();p.wait()

try:
 probe=ROOT/'.local/steamvr-probe/bin/prim-steamvr-probe'
 if a.peek:
  reference_binary=out/'prim-reference-game'
  shutil.copy2(ROOT/'.local/godot/bin/godot',reference_binary)
  reference=[reference_binary,'--path',ROOT/'project','--display-driver','x11','--script','res://tests/steamvr_reference.gd']
  game=launch(reference,env|{'PRIM_XR_SCENE':'1','PRIM_REFERENCE_STOP':str(out/'reference.stop'),'XDG_DATA_HOME':str(out/'profile-reference')},'game-first')
 else: game=launch([probe,'scene'],env,'game-first')
 deadline=time.monotonic()+15
 ready='REFERENCE_READY' if a.peek else 'init=0'
 while ready not in (out/'game-first.log').read_text() and game.poll() is None and time.monotonic()<deadline:time.sleep(.1)
 if ready not in (out/'game-first.log').read_text():raise RuntimeError('Reference game did not initialize')
 command=[ROOT/'.local/godot/bin/godot','--path',ROOT/'project','--display-driver','x11','--script',('res://tests/steamvr_peek.gd' if a.peek else 'res://tests/steamvr_companion.gd'),'--','--desktop']
 app=launch(command,env|{'PRIM_TEST_ROLE':'prim','PRIM_TEST_OUTPUT':str(out/'prim'),'XDG_DATA_HOME':str(out/'profile-prim')},'prim')
 handled=set();deadline=time.monotonic()+(400 if os.environ.get('PRIM_PEEK_INTERACTIVE')=='1' else 180)
 while app.poll() is None and time.monotonic()<deadline:
  marker=out/'prim.stage';stage=marker.read_text() if marker.exists() else ''
  if stage and stage not in handled:
   if stage=='observer':
    observer=launch(command,env|{'PRIM_TEST_ROLE':'observer','PRIM_TEST_OUTPUT':str(out/'observer'),'PRIM_TEST_HOST':(out/'prim.endpoint').read_text(),'XDG_DATA_HOME':str(out/'profile-observer')},'observer')
   elif stage=='stop-game':
    if a.peek:
     (out/'reference.stop').touch();game.wait(timeout=15)
    else: stop(game)
   elif stage=='start-game':
    if a.peek:
     (out/'reference.stop').unlink(missing_ok=True)
     game=launch(reference,env|{'PRIM_XR_SCENE':'1','PRIM_REFERENCE_STOP':str(out/'reference.stop'),'XDG_DATA_HOME':str(out/'profile-reference')},'game-second')
    else: game=launch([probe,'scene'],env,'game-second')
   else:raise RuntimeError('Unknown stage '+stage)
   handled.add(stage);(out/('prim.'+stage)).touch()
  time.sleep(.1)
 if app.poll() is None:raise TimeoutError('Companion test timed out')
 report=json.loads((out/'prim.json').read_text())
 (out/'observer.stop').touch()
 observer.wait(timeout=15)
 print(json.dumps(report,indent=2),flush=True)
 for name in ('prim','observer'):
  text=(out/(name+'.log')).read_text()
  if any(s in text for s in ('SCRIPT ERROR','ERROR:','drivers/vulkan/','servers/rendering/','ObjectDB instances leaked')):
   raise RuntimeError('Unexpected error in '+name+'.log')
 if app.returncode or report['failures']:raise SystemExit(1)
finally:
 (out/'reference.stop').touch()
 if a.peek and game and game.poll() is None:
  try: game.wait(timeout=15)
  except subprocess.TimeoutExpired: pass
 (out/'observer.stop').touch()
 for p in reversed(processes):stop(p)
 for log in logs:log.close()
