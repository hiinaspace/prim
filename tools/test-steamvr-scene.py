#!/usr/bin/env python3
"""Live SteamVR scene/eviction gate; deliberately replaces the current VR scene.

Start native SteamVR first, wear/move the headset, and pass --allow-scene-takeover.
This is a Linux diagnostic, not the Windows friends qualification.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--allow-scene-takeover', action='store_true', required=True)
parser.add_argument('--godot', type=Path, default=ROOT / '.local/godot/bin/godot')
parser.add_argument('--runtime', type=Path, default=Path.home() / '.local/share/Steam/steamapps/common/SteamVR')
args = parser.parse_args()
probe = ROOT / '.local/steamvr-probe/bin/prim-steamvr-probe'
if not probe.exists():
    parser.error('Build the disposable probe with ./tools/build-steamvr-probe.sh first.')
if not args.godot.exists() or not (args.runtime / 'steamxr_linux64.json').exists():
    parser.error('Patched Godot or SteamVR runtime missing.')
out = ROOT / '.local/steamvr-tests' / time.strftime('%Y%m%d-%H%M%S')
out.mkdir(parents=True)
print('Artifacts:', out, flush=True)
env = os.environ | {
    'DISPLAY': os.environ.get('DISPLAY', ':0'),
    'VR_OVERRIDE': str(args.runtime.resolve()),
    'XR_RUNTIME_JSON': str(args.runtime.resolve() / 'steamxr_linux64.json'),
    'XDG_DATA_HOME': str(out / 'profile'),
    'PRIM_STEAMVR_TEST_OUTPUT': str(out),
    'PRIM_NETWORK_LOCAL_ONLY': '1',
    'PRIM_XR_SCENE': '1',
    'LIBMPV_ZERO_MPV_LIBRARY': str(ROOT / '.local/mpv/lib/libmpv.so.2'),
}
for key in ('PRIM_AUTOJOIN', 'PRIM_MEDIA', 'PRIM_RUNTIME_VRM'):
    env.pop(key, None)


def stop(process):
    if process and process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


prim = game = None
try:
    with (out / 'prim.log').open('w') as log, (out / 'game.log').open('w') as game_log:
        prim = subprocess.Popen([
            'steam-run', str(args.godot.resolve()), '--path', str(ROOT / 'project'),
            '--display-driver', 'x11', '--script', 'res://tests/steamvr_scene.gd',
            '--', '--desktop',
        ], env=env, stdout=log, stderr=subprocess.STDOUT)
        deadline = time.monotonic() + 45
        while prim.poll() is None and not (out / 'prim-scene.ready').exists() and time.monotonic() < deadline:
            time.sleep(.1)
        if not (out / 'prim-scene.ready').exists():
            raise RuntimeError(f'Prim failed to enter SteamVR; inspect {out / "prim.log"}')
        game = subprocess.Popen(['steam-run', str(probe), 'scene'], env=env, stdout=game_log, stderr=subprocess.STDOUT)
        game.wait(timeout=75)
        if game.returncode:
            raise RuntimeError(f'Disposable game failed: {game.returncode}')
        (out / 'game.done').write_text('done')
        prim.wait(timeout=90)
    result = json.loads((out / 'prim-scene.result').read_text())
    log_text = (out / 'prim.log').read_text()
    if prim.returncode or result or any(marker in log_text for marker in (
        'SCRIPT ERROR', 'drivers/vulkan/', 'servers/rendering/', 'ObjectDB instances leaked',
    )):
        raise RuntimeError(f'SteamVR gate failed: {result}; inspect {out / "prim.log"}')
    print('Passed scene entry, stereo/IPD, eviction survival, re-entry and orderly disable.', flush=True)
finally:
    stop(game)
    stop(prim)
