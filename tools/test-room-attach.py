#!/usr/bin/env python3
"""C1: the Godot client attaches to a room that `prim ui` (the room window) already started.

`prim ui` runs under a private headless weston with PRIM_UI_ACT=create, which starts a daemon
and founds a room exactly as the Create button does. A headless Godot (role attach) then
connects to that daemon's room.sock without hosting or joining, must show the name prim ui
gave the member (the Godot default name must not overwrite it), mints an invite, sees a
second daemon join with it, and renames itself. Needs `prim` built with the ui feature.
"""
import json, os, shutil, socket, subprocess, sys, time
from pathlib import Path


def kill_daemons_under(path):
    """Stop `prim room` daemons this run started (they outlive their parent on purpose):
    those whose command line mentions our private output directory."""
    me = os.getpid()
    for d in Path("/proc").iterdir():
        if not d.name.isdigit() or int(d.name) == me:
            continue
        try:
            cmd = (d / "cmdline").read_bytes().replace(b"\0", b" ").decode(errors="replace")
        except OSError:
            continue
        if " room " in cmd and str(path) in cmd and "prim" in cmd.split(" room ")[0]:
            try:
                os.kill(int(d.name), 15)
            except OSError:
                pass

ROOT = Path(__file__).resolve().parents[1]
OUT = Path(sys.argv[1] if len(sys.argv) > 1 else "/tmp/prim-room-attach")
shutil.rmtree(OUT, ignore_errors=True)
for d in ("wl", "ui", "godot", "peer"):
    (OUT / d).mkdir(parents=True)
prim = ROOT / "target/debug/prim"
godot = os.environ.get("PRIM_GODOT")
for c in (godot, ROOT / ".local/godot/bin/godot", shutil.which("godot")):
    if c and Path(c).exists():
        godot = str(c)
        break
weston = os.environ.get("WESTON") or str(Path.home() / "code/xrprobe/.local/weston/bin/weston")
s = socket.socket(); s.bind(("127.0.0.1", 0)); port = s.getsockname()[1]; s.close()
procs = []
def run(cmd, log, env, **kw):
    p = subprocess.Popen(cmd, env=env, stdout=open(OUT / log, "w"), stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, start_new_session=True, **kw)
    procs.append(p)
    return p
try:
    wenv = {k: v for k, v in os.environ.items() if k not in ("WAYLAND_DISPLAY", "DISPLAY")} | {"XDG_RUNTIME_DIR": str(OUT / "wl")}
    run([weston, "--backend=headless", "--renderer=gl", "--socket=a", "--width=1280", "--height=720", "--idle-time=0"], "weston.log", wenv)
    for _ in range(100):
        if (OUT / "wl/a").exists(): break
        time.sleep(0.1)
    sock = OUT / "ui/room.sock"
    uenv = wenv | {"WAYLAND_DISPLAY": str(OUT / "wl/a"), "PRIM_SOCKET": str(sock), "PRIM_UI_ACT": "create",
                   "PRIM_UI_EXE": str(prim), "XDG_STATE_HOME": str(OUT / "ui/state"),
                   "PRIM_UI_DAEMON_ARGS": f"--relay none --bind 127.0.0.1:{port} --state-dir {OUT}/ui/room-state"}
    run([str(prim), "ui"], "ui.log", uenv)
    for _ in range(200):
        if sock.exists(): break
        time.sleep(0.1)
    else:
        sys.exit("prim ui did not start a daemon; see " + str(OUT / "ui.log"))
    time.sleep(1.0)
    # What name did prim ui give itself? (its default is the host name)
    want = subprocess.run(["hostname"], capture_output=True, text=True).stdout.strip()
    genv = os.environ | {"XDG_DATA_HOME": str(OUT / "godot/data"), "ROOM_TEST_EXPECT_NAME": want, "PRIM_BIN": str(prim)}
    gcmd = [godot, "--headless", "--path", str(ROOT / "client"), "--xr-mode", "off", "--", "--desktop", "--role", "attach",
            "--test-dir", str(OUT), "--socket", str(sock), "--name", "Godot-default"]
    g = run(gcmd, "godot.log", genv)
    for _ in range(300):
        f = OUT / "attach.json"
        if f.exists() or g.poll() is not None: break
        time.sleep(0.1)
    info = json.loads(f.read_text())
    penv = os.environ | {"XDG_STATE_HOME": str(OUT / "peer/state")}
    run([str(prim), "room", "join", info["token"], "--socket", str(OUT / "peer/room.sock"), "--rt-socket", str(OUT / "peer/rt.sock"),
         "--relay", "none", "--bind", "127.0.0.1:0", "--peer-addr", f"{info['key']}=127.0.0.1:{port}"], "peer.log", penv)
    code = g.wait(timeout=90)
finally:
    for p in procs:
        if p.poll() is None:
            p.terminate()
            try: p.wait(5)
            except subprocess.TimeoutExpired: p.kill()
kill_daemons_under(OUT)
rp = OUT / "attach.report.json"
bad = 1
if rp.exists():
    r = json.loads(rp.read_text())
    for c in r["checks"]:
        print(f"  {'PASS' if c['pass'] else 'FAIL'} {c['name']}  {json.dumps(c['detail'])}")
    bad = r["failures"]
print(f"logs: {OUT}")
print("ROOM_ATTACH_PASSED" if not bad else f"ROOM_ATTACH_FAILED ({bad})")
sys.exit(1 if bad else 0)
