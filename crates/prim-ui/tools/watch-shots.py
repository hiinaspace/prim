#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# ///
"""Watch together row of the room window on a private display (headless sway + grim),
and the window-launched `prim watch` checked against a second player.

Two states: the row idle (with the link entry), and after the window started `prim watch`
for a local test clip (mpv with null video and audio, never a real output). Then a second
daemon's `prim watch` joins and both players' positions are read over mpv's IPC.

    crates/prim-ui/tools/watch-shots.py [--prim target/debug/prim] [--out DIR] [--media FILE]
"""
import argparse, json, os, re, shutil, signal, subprocess, sys, tempfile, time
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument("--prim", default="target/debug/prim")
ap.add_argument("--out", default="/tmp/prim-pm-shots")
ap.add_argument("--size", default="1100x720")
ap.add_argument("--media", default="")
args = ap.parse_args()
PRIM = str(Path(args.prim).resolve())
OUT = Path(args.out); OUT.mkdir(parents=True, exist_ok=True)
ROOT = Path(tempfile.mkdtemp(prefix="pgw", dir="/tmp"))
RUN = ROOT / "run"; RUN.mkdir(mode=0o700)
procs = []

def sh(cmd, **kw):
    return subprocess.run(cmd, check=True, text=True, capture_output=True, **kw).stdout

def spawn(cmd, env=None, log=None):
    e = dict(os.environ); e.update(env or {})
    f = open(log or os.devnull, "ab")
    p = subprocess.Popen(cmd, env=e, stdout=f, stderr=subprocess.STDOUT, start_new_session=True)
    procs.append(p); return p

def cleanup():
    for p in reversed(procs):
        try: os.killpg(p.pid, signal.SIGTERM)
        except Exception: pass
    time.sleep(0.5)
    if os.environ.get('KEEP'): print('kept', ROOT)
    else: shutil.rmtree(ROOT, ignore_errors=True)

# ---- private display: headless sway
(ROOT / "sway.conf").write_text(f"output HEADLESS-1 resolution {args.size}\ndefault_border none\nfont pango:monospace 1\n")
senv = {"XDG_RUNTIME_DIR": str(RUN), "WLR_BACKENDS": "headless", "WLR_LIBINPUT_NO_DEVICES": "1",
        "WLR_RENDERER": "pixman", "WAYLAND_DISPLAY": "", "DISPLAY": ""}
senv.pop("WAYLAND_DISPLAY")
sway = spawn(["sway", "-c", str(ROOT / "sway.conf")], senv, ROOT / "sway.log")
for _ in range(100):
    socks = [p for p in RUN.glob("wayland-*") if not str(p).endswith(".lock")]
    if socks: break
    time.sleep(0.1)
else: sys.exit("sway did not start: " + (ROOT / "sway.log").read_text())
WL = socks[0].name
DISP = {"XDG_RUNTIME_DIR": str(RUN), "WAYLAND_DISPLAY": WL, "LIBGL_ALWAYS_SOFTWARE": "1",
        "WINIT_UNIX_BACKEND": "wayland"}
swaysock = next(RUN.glob("sway-ipc.*"))
def swaymsg(*a): return sh(["swaymsg", "-s", str(swaysock), *a])

# ---- nodes
def node(name):
    d = ROOT / name; (d / "state").mkdir(parents=True)
    return {"name": name, "dir": d, "sock": str(d / "room.sock"), "rt": str(d / "rt.sock"),
            "env": {"XDG_STATE_HOME": str(d / "state"), "PRIM_SOCKET": str(d / "room.sock")}}

def start_daemon(n, mode, token=None, peer=None):
    cmd = [PRIM, "room"] + ([mode, token] if token else [mode]) + [
        f"--socket={n['sock']}", f"--rt-socket={n['rt']}", "--relay=none", "--bind=127.0.0.1:0"]
    if mode == "found": cmd.append(f"--meta={n['name']}")
    if peer: cmd.append(f"--peer-addr={peer}")
    log = n["dir"] / "daemon.log"
    p = spawn(cmd, n["env"], log)
    for _ in range(150):
        if os.path.exists(n["sock"]) and "ready" in (n["dir"] / "daemon.out").read_text() if False else os.path.exists(n["sock"]):
            try:
                import socket as S
                s = S.socket(S.AF_UNIX); s.connect(n["sock"]); s.close(); break
            except OSError: pass
        time.sleep(0.1)
    else: sys.exit("daemon did not start: " + log.read_text())
    return p

def ctl(n, *a):
    return sh([PRIM, "ctl", f"--socket={n['sock']}", *a], env={**os.environ, **n["env"]}).strip()

def chat(n, text):
    # The receiving window may still be attaching; retry until it has registered the channel.
    for _ in range(10):
        r = subprocess.run([PRIM, "ctl", f"--socket={n['sock']}", "chat", "send", text],
                           env={**os.environ, **n["env"]}, capture_output=True, text=True)
        if r.returncode == 0: return
        time.sleep(1)
    sys.exit("chat send kept failing: " + r.stderr)

def status(n): return json.loads(ctl(n, "status", "--json"))

def set_name(n, name):
    ctl(n, "write", "member", "profile", json.dumps({"v": 1, "name": name}), "--persistent")

def window(n, extra=None, name=None):
    env = {**n["env"], **DISP, "PRIM_UI_VOICE_TEST": "1", "PRIM_UI_DAEMON_ARGS": "--relay=none --bind=127.0.0.1:0", **(extra or {})}
    return spawn([PRIM, "ui"], env, ROOT / f"window-{n['name']}.log")

def shot(name, wait=2.5):
    time.sleep(wait)
    path = OUT / f"{name}.png"
    sh(["grim", "-o", "HEADLESS-1", str(path)], env={**os.environ, **DISP})
    print("shot", path)

def close_window(p):
    os.killpg(p.pid, signal.SIGTERM); time.sleep(0.6)

def click(x, y):
    # egui needs a frame with the pointer hovering before it accepts the press.
    swaymsg("seat", "seat0", "cursor", "set", str(x - 3), str(y))
    time.sleep(0.3)
    swaymsg("seat", "seat0", "cursor", "set", str(x), str(y))
    time.sleep(0.4)
    swaymsg("seat", "seat0", "cursor", "press", "button1")
    time.sleep(0.15)
    swaymsg("seat", "seat0", "cursor", "release", "button1")



def mpv_get(sock, prop):
    import socket as S
    s = S.socket(S.AF_UNIX); s.settimeout(3); s.connect(sock)
    s.sendall((json.dumps({"command": ["get_property", prop], "request_id": 7}) + "\n").encode())
    buf = b""
    while True:
        buf += s.recv(4096)
        for line in buf.split(b"\n"):
            try: v = json.loads(line)
            except Exception: continue
            if v.get("request_id") == 7:
                s.close(); return v.get("data")

try:
    media = args.media
    if not media:
        media = str(ROOT / "clip.mp4")
        sh(["ffmpeg", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=320x240:rate=25",
            "-f", "lavfi", "-i", "sine=frequency=440", "-t", "60", "-c:v", "libx264", "-preset", "ultrafast",
            "-pix_fmt", "yuv420p", "-c:a", "aac", media])
    a = node("ana")
    start_daemon(a, "found")
    time.sleep(0.5); set_name(a, "ana")
    tok = ctl(a, "invite")
    akey = status(a)["me"]
    aaddr = [l for l in open(ROOT / "ana" / "daemon.log", errors="replace").read().splitlines() if "listen" in l]
    b = node("bo")
    # the founder's listen address is on its stdout, which the helper sends to the log
    import re
    addr = re.search(r"listen (\S+)", open(ROOT / "ana" / "daemon.log", errors="replace").read())
    start_daemon(b, "join", tok, peer=f"{akey}={addr.group(1)}" if addr else None)
    time.sleep(3); set_name(b, "bo")
    mpv_null = {"PRIM_WATCH_MPV_ARGS": "--vo=null --ao=null"}
    w = window(a, {**mpv_null})
    shot("w1-idle", 4.0)
    close_window(w)
    w = window(a, {**mpv_null, "PRIM_UI_DEMO": f"watch={media}"})
    shot("w2-watching", 6.0)
    # bo joins by command line (what the window's button with an empty entry runs)
    ipc_b = str(ROOT / "bo" / "mpv.sock")
    spawn([PRIM, "watch", f"--socket={b['sock']}", f"--ipc={ipc_b}"], {**mpv_null, **b["env"]}, ROOT / "bo-watch.log")
    time.sleep(5)
    import glob
    ipc_a = [p for p in glob.glob(str(ROOT / "ana" / "mpv-*.sock"))]
    print("ana ipc", ipc_a)
    for i in range(4):
        pa = mpv_get(ipc_a[0], "time-pos"); pb = mpv_get(ipc_b, "time-pos")
        print(f"positions ana={pa:.2f} bo={pb:.2f} skew={abs(pa-pb):.3f}")
        time.sleep(1)
    close_window(w)
finally:
    cleanup()
