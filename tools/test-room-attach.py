#!/usr/bin/env python3
"""C1 (retargeted in A1): the Godot client attaches to a room a session already runs.

`prim session` plus `prim ctl session create` start a daemon and found a room exactly as the
menu's Host button does, with no window and no `prim ui`. The member is given a name from
the command line side (`prim ctl write member profile`). A headless Godot (role attach) then
adopts that session and its room without hosting or joining, must show the name it was
given (the Godot default name must not overwrite it), mints an invite, sees a second daemon
join with it, and renames itself.
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
for d in ("ui", "godot", "peer"):
    (OUT / d).mkdir(parents=True)
prim = ROOT / "target/debug/prim"
godot = os.environ.get("PRIM_GODOT")
for c in (godot, ROOT / ".local/godot/bin/godot", Path.home() / "code/prim-gw/.local/godot/bin/godot", shutil.which("godot")):
    if c and Path(c).exists():
        godot = str(c)
        break
lib = ROOT / "target/debug/libprim_native.so"
dst = ROOT / "client/bin/linux/libprim_native.so"
if not lib.exists():
    sys.exit("build first: cargo build -p prim -p prim_native (missing " + str(lib) + ")")
dst.parent.mkdir(parents=True, exist_ok=True)
if dst.is_symlink() or dst.exists():
    dst.unlink()
dst.symlink_to(lib)
s = socket.socket(); s.bind(("127.0.0.1", 0)); port = s.getsockname()[1]; s.close()
procs = []
def run(cmd, log, env, **kw):
    p = subprocess.Popen(cmd, env=env, stdout=open(OUT / log, "w"), stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, start_new_session=True, **kw)
    procs.append(p)
    return p
try:
    sock = OUT / "ui/room.sock"
    senv = os.environ | {"PRIM_BIN": str(prim), "XDG_STATE_HOME": str(OUT / "ui/state"), "PRIM_SESSION_VOICE_TEST": "1"}
    run([str(prim), "session", "--socket", str(sock), "--no-voice"], "session.log", senv)
    for _ in range(100):
        if (OUT / "ui/session.sock").exists(): break
        time.sleep(0.1)
    else:
        sys.exit("prim session did not start; see " + str(OUT / "session.log"))
    ctl = lambda *a: subprocess.run([str(prim), "ctl", "--socket", str(sock), *a], env=senv, capture_output=True, text=True, timeout=60)
    r = ctl("session", "create", "Attach room", "--relay=none", f"--bind=127.0.0.1:{port}", f"--state-dir={OUT}/ui/room-state")
    if r.returncode != 0:
        sys.exit("session create failed: " + r.stdout + r.stderr)
    want = "Elsewhere"
    me = json.loads(ctl("status", "--json").stdout)["me"]
    r = ctl("write", "member", "profile", "--persistent", json.dumps({"v": 1, "name": want}))
    time.sleep(1.0)
    genv = os.environ | {"XDG_DATA_HOME": str(OUT / "godot/data"), "ROOM_TEST_EXPECT_NAME": want, "PRIM_BIN": str(prim), "XDG_STATE_HOME": str(OUT / "ui/state")}
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
    try:
        subprocess.run([str(prim), "ctl", "--socket", str(OUT / "ui/room.sock"), "session", "quit"], capture_output=True, timeout=30)
    except Exception:
        pass
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
