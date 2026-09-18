#!/usr/bin/env python3
"""Real Vulkan/OpenXR transitions with private Monado QWERTY and a second Prim peer.
No physical headset, system VR service, or default audio device is changed.
Run inside `nix develop` with GODOT pointing at the patched engine.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / '.local' / 'xr-lifecycle-tests' / time.strftime('%Y%m%d-%H%M%S')
OUT.mkdir(parents=True)
print('Artifacts:', OUT, flush=True)
processes, logs, modules = [], [], []
runtime = None
private_runtime = tempfile.mkdtemp(prefix='prim-monado-')

def command(*args):
    return subprocess.check_output(args, text=True).strip()

def wait_file(path, process, seconds=30):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if path.exists() and path.stat().st_size: return
        if process.poll() is not None: raise RuntimeError(f'Process exited before {path}; see {OUT}')
        time.sleep(.1)
    raise TimeoutError(path)

def launch(args, env, name):
    log = (OUT / (name + '.log')).open('w'); logs.append(log)
    process = subprocess.Popen(args, env=env, stdout=log, stderr=subprocess.STDOUT)
    processes.append(process)
    return process

try:
    fixture = ROOT / '.local/integration/media.mp4'
    if not fixture.exists():
        fixture = OUT / 'fixture.mp4'
        subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-f', 'lavfi', '-i', 'testsrc2=size=640x360:rate=30', '-f', 'lavfi', '-i', 'sine=frequency=330:sample_rate=48000', '-t', '60', '-c:v', 'libx264', '-preset', 'ultrafast', '-pix_fmt', 'yuv420p', '-c:a', 'aac', str(fixture)], check=True)
    long_fixture = OUT / 'media.mp4'
    subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-stream_loop', '3', '-i', str(fixture), '-c', 'copy', str(long_fixture)], check=True)
    fixture = long_fixture
    mic = 'prim_xr_mic_' + str(os.getpid()); speaker = 'prim_xr_output_' + str(os.getpid())
    for name in (mic, speaker):
        modules.append(command('pactl', 'load-module', 'module-null-sink', f'sink_name={name}', 'rate=48000', 'channels=2', 'norewinds=1'))
    tone = OUT / 'mic.wav'
    subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-f', 'lavfi', '-i', 'sine=frequency=620:sample_rate=48000', '-t', '220', '-af', 'volume=3', '-ac', '2', str(tone)], check=True)
    launch(['paplay', '--device=' + mic, str(tone)], os.environ.copy(), 'tone')
    base = os.environ.copy()
    base.update(DISPLAY=base.get('DISPLAY', ':0'), PRIM_NETWORK_LOCAL_ONLY='1', PULSE_SOURCE=mic + '.monitor', PULSE_SINK=speaker,
                PULSE_SERVER='unix:' + os.environ.get('XDG_RUNTIME_DIR', f'/run/user/{os.getuid()}') + '/pulse/native',
                PRIM_TEST_HOST_FILE=str(fixture), LIBMPV_ZERO_MPV_LIBRARY=str(ROOT / '.local/mpv/lib/libmpv.so.2'))
    for key in ('PRIM_AUTOJOIN', 'PRIM_MEDIA', 'PRIM_RUNTIME_VRM'): base.pop(key, None)
    xr_env = base | {'XDG_RUNTIME_DIR':private_runtime, 'XR_RUNTIME_JSON':'/run/current-system/sw/share/openxr/1/openxr_monado.json',
                     'PRIM_XR_SCENE':'1', 'QWERTY_ENABLE':'1', 'QWERTY_COMBINE':'0', 'XRT_COMPOSITOR_NULL':'1', 'XRT_NO_STDIN':'1', 'IPC_EXIT_ON_DISCONNECT':'0'}
    godot = os.environ.get('GODOT', str(ROOT / '.local/godot/bin/godot'))
    args = [godot, '--path', str(ROOT / 'project'), '--display-driver', 'x11', '--disable-vsync', '--max-fps', '60', '--script', 'res://tests/xr_lifecycle.gd']
    host = launch(args + ['--', '--desktop'], xr_env | {'PRIM_TEST_ROLE':'host', 'PRIM_TEST_OUTPUT':str(OUT / 'host'), 'XDG_DATA_HOME':str(OUT / 'profile-host')}, 'host')
    wait_file(OUT / 'host.endpoint', host)
    client = launch(args + ['--xr-mode', 'off', '--', '--desktop'], base | {'PRIM_TEST_ROLE':'client', 'PRIM_TEST_OUTPUT':str(OUT / 'client'), 'PRIM_TEST_HOST':(OUT / 'host.endpoint').read_text(), 'XDG_DATA_HOME':str(OUT / 'profile-client')}, 'client')
    handled = set(); deadline = time.monotonic() + 210
    while host.poll() is None and time.monotonic() < deadline:
        stage_file = OUT / 'host.stage'
        stage = stage_file.read_text() if stage_file.exists() else ''
        if stage and stage not in handled:
            if stage in ('runtime-start', 'runtime-restart'):
                runtime = launch(['monado-service'], xr_env, stage)
                socket = Path(private_runtime) / 'monado_comp_ipc'
                until = time.monotonic() + 15
                while not socket.exists() and runtime.poll() is None and time.monotonic() < until: time.sleep(.1)
                if runtime.poll() is not None or not socket.exists(): raise RuntimeError('Private Monado failed; see runtime log')
            elif stage == 'runtime-stop':
                runtime.terminate(); runtime.wait(timeout=10)
                (Path(private_runtime) / 'monado_comp_ipc').unlink(missing_ok=True)
            else: raise RuntimeError('Unknown stage: ' + stage)
            handled.add(stage); (OUT / ('host.' + stage)).write_text('ok')
        time.sleep(.1)
    if host.poll() is None: raise TimeoutError('XR lifecycle test; see ' + str(OUT))
    client.wait(timeout=15)
    reports = [json.loads((OUT / (role + '.json')).read_text()) for role in ('host', 'client')]
    print(json.dumps(reports, indent=2))
    if host.returncode or client.returncode or any(report['failures'] for report in reports): raise SystemExit(1)
    # Runtime-unavailable/loss errors are injected deliberately. Rendering and
    # script errors are never expected, even if the scene assertions pass.
    for role in ('host', 'client'):
        output = (OUT / (role + '.log')).read_text()
        unexpected = ('SCRIPT ERROR', 'servers/rendering/', 'drivers/vulkan/', 'ObjectDB instances leaked', 'RID allocations of type')
        if any(marker in output for marker in unexpected):
            raise RuntimeError(f'Unexpected rendering/script/resource error; see {OUT / (role + ".log")}')
    if not (OUT / 'host.closed-vr').exists(): raise RuntimeError('Closing from VR did not finish desktop teardown')
finally:
    for process in reversed(processes):
        if process.poll() is None:
            process.terminate()
            try: process.wait(timeout=5)
            except subprocess.TimeoutExpired: process.kill(); process.wait()
    for log in logs: log.close()
    for module in reversed(modules): subprocess.run(['pactl', 'unload-module', module], check=False)
    shutil.rmtree(private_runtime)
