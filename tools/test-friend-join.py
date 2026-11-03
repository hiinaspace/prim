#!/usr/bin/env python3
"""Three headless Godot processes: a Friends host, its listed friend and an unlisted intruder.

The guest and intruder press the Join button next to the host's name; the guest
must reach the room (pose and voice both ways), the intruder must be refused.
Isolated profiles, loopback-only networking, no display, no audio devices.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / '.local' / 'friend-join'
shutil.rmtree(OUT, ignore_errors=True)
OUT.mkdir(parents=True)
godot = os.environ.get('GODOT', str(ROOT / '.local/godot/bin/godot'))
base = os.environ.copy()
base.update(PRIM_NETWORK_LOCAL_ONLY='1', PRIM_TEST_DIR=str(OUT),
            LIBMPV_ZERO_MPV_LIBRARY=str(ROOT / '.local/mpv/lib/libmpv.so.2'))
for name in ('PRIM_AUTOJOIN', 'PRIM_MEDIA', 'PRIM_TEST_HOST'): base.pop(name, None)
command = [godot, '--headless', '--path', str(ROOT / 'project'), '--xr-mode', 'off',
           '--script', 'res://tests/friend_join.gd', '--', '--desktop']
roles = ['host', 'guest', 'intruder']
# Optional: PRIM_TEST_WINDOWS_EXE runs one role (PRIM_TEST_WINDOWS_ROLE, default guest) as the Windows package under Wine.
windows_exe = os.environ.get('PRIM_TEST_WINDOWS_EXE')
windows_role = os.environ.get('PRIM_TEST_WINDOWS_ROLE', 'guest')
processes = []
logs = []
try:
    for role in roles:
        log = open(OUT / f'{role}.log', 'w')
        logs.append(log)
        env = base | {'PRIM_TEST_ROLE': role, 'XDG_DATA_HOME': str(OUT / f'profile-{role}')}
        launch = command
        if windows_exe and role == windows_role:
            # Exported Windows test package (tools/export-test-pack.py) under Wine, isolated prefix.
            launch = ['wine', windows_exe, '--headless', '--xr-mode', 'off', '--', '--desktop']
            env['PRIM_TEST_DIR'] = 'Z:' + str(OUT).replace('/', '\\')
            env.pop('LIBMPV_ZERO_MPV_LIBRARY', None)
            env["WINEPREFIX"] = str(ROOT / ".local/wine-friend-join")
            env['WINEDLLOVERRIDES'] = 'winemenubuilder.exe,mscoree,mshtml=;vulkan-1=b'
            env['WINEDEBUG'] = '-all'
        processes.append(subprocess.Popen(launch, env=env, stdout=log, stderr=subprocess.STDOUT))
    codes = [process.wait(timeout=150) for process in processes]
    reports = {role: json.loads((OUT / f'{role}.json').read_text()) for role in roles if (OUT / f'{role}.json').exists()}
    print(json.dumps(reports, indent=2))
    ids = {role: (OUT / f'{role}.id').read_text() for role in roles if (OUT / f'{role}.id').exists()}
    assert len(set(ids.values())) == len(ids) == 3, ids
    if any(codes) or len(reports) != 3 or any(r['failures'] for r in reports.values()):
        raise SystemExit(f'friend join test failed: exit codes {codes}; logs in {OUT}')
    print('FRIEND_JOIN_PASSED')
finally:
    for process in processes:
        if process.poll() is None:
            process.terminate()
            try: process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill(); process.wait()
    for log in logs: log.close()
