#!/usr/bin/env python3
"""C1 oracle: five headless Godot clients, each with its own real `prim room` daemon.

host     founds a room from the Godot screen, names itself, mints invites, revokes the guest
guest    pastes the host's invite: must be in the room, see the host's name, and be removed
         when the host revokes it
forged   a token with one character changed: must be refused
replay   the guest's already-used token: must be refused
expired  an invite used after its 1.5 s lifetime: must be refused

Loopback only (--relay none), isolated state per role, no display and no audio devices
(prim room opens neither). Port of the old tools/test-friend-join.py to daemons.

  tools/test-room-join.py [--fault no-revoke|valid-token] [--godot PATH] [--no-build]

--fault is a negative control: the matching check must go red and the run exit non-zero.
"""
import argparse
import json
import os
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ap = argparse.ArgumentParser()
ap.add_argument("--fault", choices=["no-revoke", "valid-token"])
ap.add_argument("--godot")
ap.add_argument("--no-build", action="store_true")
ap.add_argument("--out", default=None)
ap.add_argument("--timeout", type=int, default=200)
a = ap.parse_args()

OUT = Path(a.out) if a.out else Path(os.environ.get("TMPDIR", "/tmp")) / f"prim-room-join-{os.getuid()}"
shutil.rmtree(OUT, ignore_errors=True)
OUT.mkdir(parents=True)

if not a.no_build:
    subprocess.run(["cargo", "build", "--locked", "-q", "-p", "prim", "-p", "prim_native"], cwd=ROOT, check=True)
lib = ROOT / "target/debug/libprim_native.so"
dst = ROOT / "client/bin/linux/libprim_native.so"
dst.parent.mkdir(parents=True, exist_ok=True)
if dst.is_symlink() or dst.exists():
    dst.unlink()
dst.symlink_to(lib)

godot = a.godot or os.environ.get("PRIM_GODOT")
for c in (godot, ROOT / ".local/godot/bin/godot", Path.home() / "code/prim-l0/.local/godot/bin/godot", shutil.which("godot")):
    if c and Path(c).exists():
        godot = str(c)
        break
else:
    sys.exit("no Godot binary: pass --godot or set PRIM_GODOT")

s = socket.socket()
s.bind(("127.0.0.1", 0))
port = s.getsockname()[1]
s.close()

roles = ["host", "guest", "forged", "replay", "expired"]
procs, logs = {}, []
try:
    for role in roles:
        rd = OUT / role
        (rd / "data").mkdir(parents=True)
        (rd / "state").mkdir()
        env = os.environ.copy()
        env.update(
            XDG_DATA_HOME=str(rd / "data"), XDG_STATE_HOME=str(rd / "state"),
            XDG_RUNTIME_DIR=str(rd),
            PRIM_BIN=str(ROOT / "target/debug/prim"),
            PRIM_DAEMON_ARGS="--relay none --bind " + (f"127.0.0.1:{port}" if role == "host" else "127.0.0.1:0"),
            ROOM_TEST_PORT=str(port),
        )
        env.pop("PRIM_SOCKET", None)
        env.pop("PRIM_EVENT_LOG", None)
        if a.fault:
            env["ROOM_TEST_FAULT"] = a.fault
        log = open(OUT / f"{role}.log", "w")
        logs.append(log)
        cmd = [godot, "--headless", "--path", str(ROOT / "client"), "--xr-mode", "off", "--",
               "--desktop", "--role", role, "--test-dir", str(OUT), "--socket", str(rd / "room.sock")]
        procs[role] = subprocess.Popen(cmd, env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
    deadline = time.time() + a.timeout
    codes = {}
    for role, p in procs.items():
        try:
            codes[role] = p.wait(timeout=max(1, deadline - time.time()))
        except subprocess.TimeoutExpired:
            codes[role] = "timeout"
finally:
    for p in procs.values():
        if p.poll() is None:
            p.terminate()
            try:
                p.wait(timeout=5)
            except subprocess.TimeoutExpired:
                p.kill()
    for l in logs:
        l.close()

bad = 0
for role in roles:
    rp = OUT / f"{role}.report.json"
    if not rp.exists():
        print(f"{role}: NO REPORT (exit {codes.get(role)}); see {OUT}/{role}.log")
        bad += 1
        continue
    r = json.loads(rp.read_text())
    for c in r["checks"]:
        print(f"  {'PASS' if c['pass'] else 'FAIL'} [{role}] {c['name']}")
    bad += r["failures"]
ids = {}
for role in ("host", "guest"):
    f = OUT / f"{role}.json"
    if f.exists():
        ids[role] = json.loads(f.read_text()).get("key")
if len({v for v in ids.values() if v}) != 2:
    print("FAIL host and guest do not have two distinct identities", ids)
    bad += 1
print(f"logs and reports: {OUT}")
if bad:
    print(f"ROOM_JOIN_FAILED ({bad} problems){' (expected: fault ' + a.fault + ')' if a.fault else ''}")
    sys.exit(1)
print("ROOM_JOIN_PASSED")
