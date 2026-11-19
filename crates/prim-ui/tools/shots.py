#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# ///
"""Screenshot run of `prim ui` on a private display (headless sway + grim).

Builds each state of the window (empty, alone, two members, voice on, chat, advanced)
with real daemons on loopback under a private XDG_STATE_HOME and private sockets, and
saves one PNG per state. Voice uses test sinks (tone microphone, null output): never real
speakers or a microphone. Nothing here touches the desktop's display or the user's rooms.

    crates/prim-ui/tools/shots.py [--prim target/debug/prim] [--out DIR]
"""
import argparse, json, os, re, shutil, signal, subprocess, sys, tempfile, time
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument("--prim", default="target/debug/prim")
ap.add_argument("--out", default="/tmp/prim-gw-shots")
ap.add_argument("--size", default="1100x720")
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

try:
    only = set(sys.argv[1:]) if False else None
    # 1. empty: no daemon
    a = node("ana")
    w = window(a); shot("1-empty"); close_window(w)

    # 2. in a room alone
    start_daemon(a, "found")
    time.sleep(0.5); set_name(a, "ana")
    w = window(a); shot("2-alone")

    # 3. two members
    b = node("ben")
    tok = ctl(a, "invite", "--expiry", "1h")
    listen = None
    # find a's listen address from its log
    for line in (a["dir"] / "daemon.log").read_text().splitlines():
        if line.startswith("listen 127.0.0.1"): listen = line.split()[1]
    me = status(a)["me"]
    start_daemon(b, "join", tok, peer=f"{me}={listen}")
    for _ in range(100):
        s = status(b)
        if len(s["members"]) >= 2 and all(m["online"] for m in s["members"]): break
        time.sleep(0.2)
    set_name(b, "ben")
    shot("3-two-members", 3)

    # 4. voice on: ben has voice (muted mic), ana's window starts it with a click
    spawn([PRIM, "voice", f"--rt-socket={b['rt']}", "--capture=none", "--output=null"], b["env"], b["dir"] / "voice.log")
    time.sleep(1.5)
    shot("4a-friend-in-voice", 1)
    # No pointer on a headless display: PRIM_UI_DEMO presses the toolbar buttons.
    close_window(w)
    w = window(a, {"PRIM_UI_DEMO": "voice"}); shot("4b-voice-on", 3.5)
    close_window(w)
    w = window(a, {"PRIM_UI_DEMO": "voice,mic"}); shot("4c-mic-live", 3.5)
    close_window(w)
    w = window(a, {"PRIM_UI_DEMO": "voice,mic,deaf"}); shot("4d-deafened", 3.5)
    close_window(w)
    w = window(a)

    # 5. chat (ben has no window here, so a `chat tail` registers the channel for it)
    spawn([PRIM, "ctl", f"--socket={b['sock']}", "chat", "tail", "--timeout", "90"], b["env"], b["dir"] / "tail.log")
    time.sleep(1)
    ctl(b, "chat", "send", "hey, can you hear me?")
    time.sleep(0.4)
    print(sh([PRIM, "ctl", f"--socket={a['sock']}", "chat", "send", "test"], env={**os.environ, **a["env"]}) if False else subprocess.run([PRIM, "ctl", f"--socket={a['sock']}", "chat", "send", "test"], env={**os.environ, **a["env"]}, capture_output=True, text=True))
    time.sleep(0.4)
    ctl(b, "chat", "send", "yes! the voice dot went green on my side")
    shot("5-chat", 1.5)
    close_window(w)

    # 6. advanced
    w = window(a, {"PRIM_UI_ADVANCED": "1"}); shot("6-advanced", 3.5)
    close_window(w)
finally:
    cleanup()
