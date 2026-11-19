#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# ///
"""Two `prim ui` windows, two private daemons, one room, on a private headless sway display.

Window A creates the room and says "test"; window B joins with the invite and answers.
Both windows use the same app actions as their buttons (PRIM_UI_ACT: create / join= / say=),
since a headless display has no pointer. The invite token is minted over the local API
(the Invite button copies it to the clipboard, which a script cannot read). Voice and
audio are not used. Writes one screenshot of both windows and prints what each side shows.

    crates/prim-ui/tools/two-windows.py [--prim target/debug/prim] [--out DIR]
"""
import argparse, json, os, re, shutil, signal, subprocess, sys, tempfile, time
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument("--prim", default="target/debug/prim")
ap.add_argument("--out", default="/tmp/prim-gw-two")
args = ap.parse_args()
PRIM = str(Path(args.prim).resolve())
OUT = Path(args.out); OUT.mkdir(parents=True, exist_ok=True)
ROOT = Path(tempfile.mkdtemp(prefix="pgt", dir="/tmp"))
RUN = ROOT / "run"; RUN.mkdir(mode=0o700)
procs = []

def sh(cmd, **kw): return subprocess.run(cmd, check=True, text=True, capture_output=True, **kw).stdout
def spawn(cmd, env, log):
    p = subprocess.Popen(cmd, env={**os.environ, **env}, stdout=open(log, "ab"), stderr=subprocess.STDOUT, start_new_session=True)
    procs.append(p); return p
def cleanup():
    for p in reversed(procs):
        try: os.killpg(p.pid, signal.SIGTERM)
        except Exception: pass
    time.sleep(0.5)
    # daemons/voice started by the windows are in their own groups: stop them by socket
    for n in ("a", "b"):
        for pid in os.listdir("/proc"):
            if pid.isdigit():
                try: cmd = open(f"/proc/{pid}/cmdline").read()
                except OSError: continue
                if str(ROOT) in cmd and "prim" in cmd: 
                    try: os.kill(int(pid), signal.SIGTERM)
                    except OSError: pass
    time.sleep(0.3)
    if os.environ.get("KEEP"): print("kept", ROOT)
    else: shutil.rmtree(ROOT, ignore_errors=True)

(ROOT / "sway.conf").write_text("output HEADLESS-1 resolution 1600x760\ndefault_border none\n")
senv = {"XDG_RUNTIME_DIR": str(RUN), "WLR_BACKENDS": "headless", "WLR_LIBINPUT_NO_DEVICES": "1", "WLR_RENDERER": "pixman"}
env0 = dict(os.environ); env0.pop("WAYLAND_DISPLAY", None)
sway = subprocess.Popen(["sway", "-c", str(ROOT / "sway.conf")], env={**env0, **senv}, stdout=open(ROOT / "sway.log", "ab"), stderr=subprocess.STDOUT, start_new_session=True)
procs.append(sway)
try:
    for _ in range(100):
        socks = [p for p in RUN.glob("wayland-*") if not str(p).endswith(".lock")]
        if socks: break
        time.sleep(0.1)
    else: sys.exit("sway did not start")
    DISP = {"XDG_RUNTIME_DIR": str(RUN), "WAYLAND_DISPLAY": socks[0].name, "LIBGL_ALWAYS_SOFTWARE": "1", "WINIT_UNIX_BACKEND": "wayland"}

    def node(name):
        d = ROOT / name; (d / "state").mkdir(parents=True)
        return {"dir": d, "sock": str(d / "room.sock"),
                "env": {"XDG_STATE_HOME": str(d / "state"), "PRIM_SOCKET": str(d / "room.sock"),
                        "PRIM_UI_NAME": name, "PRIM_UI_VOICE_TEST": "1", **DISP}}
    a, b = node("alpha"), node("beta")

    def ctl(n, *x): return sh([PRIM, "ctl", f"--socket={n['sock']}", *x], env={**os.environ, **n["env"]}).strip()
    def wait(f, what, secs=40):
        for _ in range(secs * 5):
            try:
                v = f()
                if v: return v
            except Exception: pass
            time.sleep(0.2)
        sys.exit("timed out: " + what)

    # Window A: creates the room, then says "test" (daemon on loopback, no relay).
    a["env"].update({"PRIM_UI_DAEMON_ARGS": "--relay=none --bind=127.0.0.1:0", "PRIM_UI_ACT": "create;say=test"})
    spawn([PRIM, "ui"], a["env"], ROOT / "window-a.log")
    wait(lambda: json.loads(ctl(a, "status", "--json"))["members"], "alpha's room")
    st = json.loads(ctl(a, "status", "--json"))
    listen = wait(lambda: next((l.split()[1] for l in (a["dir"] / "state/prim/ui/daemon.log").read_text().splitlines() if l.startswith("listen ")), None), "alpha listen addr")
    tok = ctl(a, "invite", "--expiry", "1h")
    # Window B: joins with the invite, then answers.
    b["env"].update({"PRIM_UI_DAEMON_ARGS": f"--relay=none --bind=127.0.0.1:0 --peer-addr={st['me']}={listen}",
                     "PRIM_UI_ACT": f"join={tok};say=hello alpha, this is beta"})
    spawn([PRIM, "ui"], b["env"], ROOT / "window-b.log")
    def both():
        for n in (a, b):
            s = json.loads(ctl(n, "status", "--json"))
            if len(s["members"]) < 2 or not all(m["online"] for m in s["members"]): return False
        return True
    wait(both, "both members online in both windows' daemons")
    # Let the chat lines land; then read them back from each side with `chat tail`.
    time.sleep(4)
    for n, who in ((a, "alpha"), (b, "beta")):
        out = subprocess.run([PRIM, "ctl", f"--socket={n['sock']}", "chat", "send", f"(probe from {who}'s CLI)"], env={**os.environ, **n["env"]}, capture_output=True, text=True)
        print(who, "cli probe:", out.stdout.strip())
    time.sleep(1.5)
    path = OUT / "two-windows.png"
    sh(["grim", "-o", "HEADLESS-1", str(path)], env={**os.environ, **DISP})
    print("shot", path)
finally:
    cleanup()
