#!/usr/bin/env python3
"""Private Monado/QWERTY real-session companion test; no host XR service changes."""
import json, os, subprocess, tempfile, time
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
OUT=ROOT/'.local/xr-companion'/time.strftime('coexistence-%Y%m%d-%H%M%S')
OUT.mkdir(parents=True)
processes=[]; logs=[]; runtime=None; game=None; app=None; observer=None
private=tempfile.TemporaryDirectory(prefix='prim-companion-')
def launch(args,env,name):
    log=(OUT/(name+'.log')).open('w'); logs.append(log)
    p=subprocess.Popen(args,env=env,stdout=log,stderr=subprocess.STDOUT)
    processes.append(p); return p
def wait_file(path,p,seconds=25):
    end=time.monotonic()+seconds
    while not path.exists() and p.poll() is None and time.monotonic()<end: time.sleep(.1)
    if not path.exists(): raise RuntimeError(f'Missing {path}; process status {p.poll()}')
try:
    env=os.environ.copy()
    env.update(XDG_RUNTIME_DIR=private.name, XR_RUNTIME_JSON='/run/current-system/sw/share/openxr/1/openxr_monado.json',
        DISPLAY=env.get('DISPLAY',':0'), QWERTY_ENABLE='1', QWERTY_COMBINE='0', XRT_COMPOSITOR_NULL='1',
        XRT_NO_STDIN='1', IPC_EXIT_ON_DISCONNECT='0', PRIM_NETWORK_LOCAL_ONLY='1',
        PULSE_SERVER='unix:'+os.environ.get('XDG_RUNTIME_DIR',f'/run/user/{os.getuid()}')+'/pulse/native',
        LIBMPV_ZERO_MPV_LIBRARY=str(ROOT/'.local/mpv/lib/libmpv.so.2'))
    for key in ('PRIM_AUTOJOIN','PRIM_MEDIA','PRIM_RUNTIME_VRM','PRIM_XR_SCENE'): env.pop(key,None)
    runtime=launch(['monado-service'],env,'runtime')
    wait_file(Path(private.name)/'monado_comp_ipc',runtime)
    godot=os.environ.get('GODOT',str(ROOT/'.local/godot/bin/godot'))
    args=[godot,'--path',str(ROOT/'project'),'--display-driver','x11','--disable-vsync','--max-fps','60','--script','res://tests/xr_companion.gd','--','--desktop']
    app=launch(args,env|{'PRIM_TEST_ROLE':'prim','PRIM_TEST_OUTPUT':str(OUT/'prim'),'XDG_DATA_HOME':str(OUT/'profile-prim')},'prim')
    handled=set(); end=time.monotonic()+150
    while app.poll() is None and time.monotonic()<end:
        stage=(OUT/'prim.stage').read_text() if (OUT/'prim.stage').exists() else ''
        if stage and stage not in handled:
            if stage=='start-observer':
                observer=launch(args,env|{'PRIM_TEST_ROLE':'observer','PRIM_TEST_OUTPUT':str(OUT/'observer'),'PRIM_TEST_HOST':(OUT/'prim.endpoint').read_text(),'XDG_DATA_HOME':str(OUT/'profile-observer')},'observer')
            elif stage=='start-game':
                game=launch(args,env|{'PRIM_XR_SCENE':'1','PRIM_TEST_ROLE':'game','PRIM_TEST_OUTPUT':str(OUT/'game'),'XDG_DATA_HOME':str(OUT/'profile-game')},'game')
                wait_file(OUT/'game.ready',game)
            elif stage=='stop-game':
                (OUT/'game.stop').touch(); game.wait(timeout=15)
                if game.returncode: raise RuntimeError('Reference game failed')
            else: raise RuntimeError(stage)
            handled.add(stage); (OUT/('prim.'+stage)).touch()
        time.sleep(.1)
    if app.poll() is None: raise TimeoutError('Prim coexistence probe')
    report=json.loads((OUT/'prim.json').read_text())
    print(json.dumps(report,indent=2))
    (OUT/'observer.stop').touch()
    observer.wait(timeout=15)
    for name in ('prim','game','observer'):
        text=(OUT/(name+'.log')).read_text()
        bad=('SCRIPT ERROR','servers/rendering/','drivers/vulkan/','ObjectDB instances leaked','RID allocations of type','ERROR:')
        if any(marker in text for marker in bad): raise RuntimeError(f'Rendering/script/resource error: {name}.log')
    if app.returncode or report['failures']: raise SystemExit(1)
finally:
    if observer and observer.poll() is None:
        (OUT/'observer.stop').touch()
        try: observer.wait(timeout=5)
        except subprocess.TimeoutExpired: pass
    # Request the reference game's orderly XR exit before stopping the private service.
    if game and game.poll() is None:
        (OUT/'game.stop').touch()
        try: game.wait(timeout=15)
        except subprocess.TimeoutExpired: pass
    for p in reversed(processes):
        if p.poll() is None:
            p.terminate()
            try: p.wait(timeout=10)
            except subprocess.TimeoutExpired: p.kill(); p.wait()
    for log in logs: log.close()
    private.cleanup()
    print('Artifacts:',OUT,flush=True)
