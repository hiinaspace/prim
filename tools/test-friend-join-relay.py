#!/usr/bin/env python3
"""Join-on-name over the iroh relay: listed friend joins by bare id, a stranger is refused.

Same roles as test-friend-join.py, but nothing is local-only: the host publishes to n0 address
lookup and the guest/intruder know only its endpoint id. Guest and intruder run in an unprivileged
pasta netns (tools/relay-netns.sh) that drops UDP (except DNS) and all LAN traffic, so the only
path to the host is the public relay over TCP 443. Needs nix-shell, iptables and internet.
This simulates "different networks" on one LAN machine; it is not two real networks.
"""
import json, os, shutil, subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / '.local' / 'friend-join-relay'
shutil.rmtree(OUT, ignore_errors=True); OUT.mkdir(parents=True)
godot = os.environ.get('GODOT', str(ROOT / '.local/godot/bin/godot'))
base = os.environ.copy()
base.update(PRIM_TEST_DIR=str(OUT), PRIM_TEST_BARE_ID='1',
            LIBMPV_ZERO_MPV_LIBRARY=str(ROOT / '.local/mpv/lib/libmpv.so.2'))
for name in ('PRIM_AUTOJOIN', 'PRIM_MEDIA', 'PRIM_TEST_HOST', 'PRIM_NETWORK_LOCAL_ONLY'): base.pop(name, None)
command = [godot, '--headless', '--path', str(ROOT / 'project'), '--xr-mode', 'off',
           '--script', 'res://tests/friend_join.gd', '--', '--desktop']
roles = ['host', 'guest', 'intruder']
procs, logs = [], []
try:
    for role in roles:
        log = open(OUT / f'{role}.log', 'w'); logs.append(log)
        env = base | {'PRIM_TEST_ROLE': role, 'XDG_DATA_HOME': str(OUT / f'profile-{role}')}
        launch = command if role == 'host' else [str(ROOT / 'tools/relay-netns.sh')] + command
        procs.append(subprocess.Popen(launch, env=env, stdout=log, stderr=subprocess.STDOUT))
    codes = [p.wait(timeout=300) for p in procs]
    reports = {r: json.loads((OUT / f'{r}.json').read_text()) for r in roles if (OUT / f'{r}.json').exists()}
    print(json.dumps(reports, indent=2))
    if any(codes) or len(reports) != 3 or any(r['failures'] for r in reports.values()):
        raise SystemExit(f'relay join test failed: exit codes {codes}; logs in {OUT}')
    print('RELAY_JOIN_PASSED (guest joined by bare id on a relay-only path; stranger refused)')
finally:
    for p in procs:
        if p.poll() is None:
            p.terminate()
            try: p.wait(timeout=5)
            except subprocess.TimeoutExpired: p.kill(); p.wait()
    for l in logs: l.close()
