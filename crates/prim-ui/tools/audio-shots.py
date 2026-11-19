#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# ///
"""Screenshots of the Audio panel on a private display (headless sway + grim).

Five states: idle, meter moving on a tone source, test recording, test playing back, saved
device missing. A tone "microphone" and a null output only; the device list is a fixed
fake, so no real device is enumerated or opened.

    crates/prim-ui/tools/audio-shots.py [--prim target/debug/prim] [--out DIR]
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


FAKE = json.dumps({
    "inputs": [{"id": "pw:usb", "name": "USB Microphone (Blue Yeti)"}, {"id": "pw:built", "name": "Built-in Audio Analog Stereo"}],
    "outputs": [{"id": "pw:hp", "name": "Headphones (USB Audio)"}, {"id": "pw:built", "name": "Built-in Audio Analog Stereo"}],
    "default_input": {"id": "pw:built", "name": "Built-in Audio Analog Stereo"},
    "default_output": {"id": "pw:built", "name": "Built-in Audio Analog Stereo"}})

def settings(name, **kw):
    p = ROOT / f"{name}.json"
    p.write_text(json.dumps({"v": 1, "input": kw.get("input"), "output": kw.get("output"),
                             "gain_db": kw.get("gain", 0.0), "volume": 1.0}))
    return str(p)

def panel(n, state, demo, tone, cfg, wait):
    w = window(n, {"PRIM_UI_DEMO": demo, "PRIM_UI_AUDIO_TONE": tone, "PRIM_AUDIO_SETTINGS": cfg,
                   "PRIM_UI_AUDIO_DEVICES": FAKE})
    shot(state, wait)
    try:
        import struct; b=open(n['dir']/'meter.levels','rb').read(); print(state, struct.unpack('<Iffff', b[4:24]))
    except Exception as e: print(state, 'levels', e)
    close_window(w)

try:
    a = node("ana")
    start_daemon(a, "found")
    time.sleep(0.5); set_name(a, "ana")
    mic = {"id": "pw:usb", "name": "USB Microphone (Blue Yeti)"}
    spk = {"id": "pw:hp", "name": "Headphones (USB Audio)"}
    panel(a, "a1-idle", "audio", "0", settings("idle", input=mic, output=spk), 3.5)
    panel(a, "a2-meter-tone", "audio", "440", settings("tone", input=mic, output=spk, gain=6.0), 3.5)
    # the test starts 1.5 s after the panel opens; recording lasts 5 s, then playback
    panel(a, "a3-recording", "audiotest", "440", settings("rec", input=mic, output=spk, gain=6.0), 4.5)
    panel(a, "a4-playing", "audiotest", "440", settings("play", input=mic, output=spk, gain=6.0), 9.0)
    gone = {"id": "pw:gone", "name": "USB Microphone (Rode NT-USB)"}
    panel(a, "a5-missing-device", "audio", "440", settings("gone", input=gone, output=spk, gain=3.0), 3.5)
finally:
    cleanup()
