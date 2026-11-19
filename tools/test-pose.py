#!/usr/bin/env python3
"""C2 oracle: avatars and published poses, against real daemons and `prim-bot`, headless.

Two `prim room` daemons (A founds, B joins by invite; loopback, no relay), a bot on B that
publishes a known moving pose (a function of time, see prim-bot.rs) with its LEFT hand
raised, and a headless Godot on A (client/avatar/pose_test.gd). Scenarios:

  main   avatar tracks the bot's trajectory within 1 cm, trails by <= 0.35 s, moves
         smoothly, left/right hands on the correct sides, name label; and what Godot
         published (read back by the bot from daemon B) follows the camera, which is moved
         mid-run, with the right flags
  stop   the bot stops publishing but stays attached: the avatar must vanish
  kill   daemon B is killed: the avatar must vanish

  tools/test-pose.py [--only main|stop|kill] [--fault nointerp|mirror-x|stale] [--godot PATH] [--no-build]

--fault is a negative control for `main` (and `stale` for stop/kill is not meaningful): the
matching check must go red and the run must exit non-zero.
"""
import argparse, json, math, os, re, shutil, signal, subprocess, sys, time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ap = argparse.ArgumentParser()
ap.add_argument("--only", choices=["main", "stop", "kill"])
ap.add_argument("--fault", choices=["nointerp", "mirror-x", "stale"])
ap.add_argument("--godot")
ap.add_argument("--shots", help="open a real window (needs a display) and save host-view stills into this dir")
ap.add_argument("--no-build", action="store_true")
ap.add_argument("--out", default=None)
a = ap.parse_args()
OUT = Path(a.out) if a.out else Path(os.environ.get("TMPDIR", "/tmp")) / f"prim-pose-{os.getuid()}"
shutil.rmtree(OUT, ignore_errors=True)
OUT.mkdir(parents=True)

if not a.no_build:
    subprocess.run(["cargo", "build", "--locked", "-q", "-p", "prim", "-p", "prim_native", "-p", "prim-client"], cwd=ROOT, check=True)
lib, dst = ROOT / "target/debug/libprim_native.so", ROOT / "client/bin/linux/libprim_native.so"
dst.parent.mkdir(parents=True, exist_ok=True)
if dst.is_symlink() or dst.exists():
    dst.unlink()
dst.symlink_to(lib)
prim, bot = ROOT / "target/debug/prim", ROOT / "target/debug/prim-bot"
godot = a.godot or os.environ.get("PRIM_GODOT")
for c in (godot, ROOT / ".local/godot/bin/godot", Path.home() / "code/prim-l0/.local/godot/bin/godot", shutil.which("godot")):
    if c and Path(c).exists():
        godot = str(c)
        break
else:
    sys.exit("no Godot binary: pass --godot or set PRIM_GODOT")


def wait_line(log: Path, pat: str, secs: float, proc=None):
    end = time.time() + secs
    while time.time() < end:
        if log.exists():
            m = re.search(pat, log.read_text(), re.M)
            if m:
                return m
        if proc and proc.poll() is not None:
            break
        time.sleep(0.1)
    sys.exit(f"timeout waiting for {pat!r} in {log}")


def qdot(p, q):
    return abs(sum(x * y for x, y in zip(p, q)))


def run_scenario(mode: str) -> dict:
    d = OUT / mode
    d.mkdir()
    procs = {}
    base = os.environ.copy()
    base.pop("PRIM_SOCKET", None)
    try:
        sa, sb = d / "a.sock", d / "b.sock"
        la = d / "daemon-a.log"
        procs["a"] = subprocess.Popen([prim, "room", "found", "--meta", "pose", "--socket", sa, "--rt-socket", d / "a-rt.sock",
                                       "--state-dir", d / "a-state", "--relay", "none", "--bind", "127.0.0.1:0"],
                                      stdout=open(la, "w"), stderr=subprocess.STDOUT, start_new_session=True)
        wait_line(la, r"^ready$", 30, procs["a"])
        key_a = re.search(r"^member (\w+)$", la.read_text(), re.M).group(1)
        listen_a = re.search(r"^listen (\S+)$", la.read_text(), re.M).group(1)
        tok = subprocess.run([prim, "ctl", "--socket", sa, "invite", "--expiry", "1h"], capture_output=True, text=True).stdout.strip().split()[-1]
        lb = d / "daemon-b.log"
        procs["b"] = subprocess.Popen([prim, "room", "join", tok, "--socket", sb, "--rt-socket", d / "b-rt.sock",
                                       "--state-dir", d / "b-state", "--relay", "none", "--bind", "127.0.0.1:0",
                                       "--peer-addr", f"{key_a}={listen_a}"],
                                      stdout=open(lb, "w"), stderr=subprocess.STDOUT, start_new_session=True)
        wait_line(lb, r"^ready$", 30, procs["b"])
        watch = d / "watch.jsonl"
        bot_args = [bot, "--socket", sb, "--name", "pose-bot", "--watch", watch]
        if mode == "stop":
            bot_args += ["--stop-after", "9"]
        procs["bot"] = subprocess.Popen(bot_args, stdout=open(d / "bot.log", "w"), stderr=subprocess.STDOUT, start_new_session=True)
        genv = base | {"XDG_DATA_HOME": str(d / "data"), "PRIM_FAULT": a.fault or ""} | ({"POSE_TEST_SHOTS": str(Path(a.shots).resolve())} if a.shots else {})
        gcmd = [godot, *([] if a.shots else ["--headless"]), "--path", str(ROOT / "client"), "--xr-mode", "off", "--", "--desktop",
                "--pose-test", mode, "--test-dir", str(d), "--socket", str(sa), "--name", "godot-me"]
        procs["godot"] = subprocess.Popen(gcmd, env=genv, stdout=open(d / "godot.log", "w"), stderr=subprocess.STDOUT, start_new_session=True)
        if mode == "kill":
            end = time.time() + 60
            while not (d / "seen.json").exists() and time.time() < end and procs["godot"].poll() is None:
                time.sleep(0.1)
            time.sleep(3)
            for k in ("bot", "b"):
                os.killpg(procs[k].pid, signal.SIGKILL)  # only processes started here
            (d / "killed.json").write_text("{}")
        try:
            procs["godot"].wait(timeout=90)
        except subprocess.TimeoutExpired:
            pass
    finally:
        for p in procs.values():
            if p.poll() is None:
                try:
                    os.killpg(p.pid, signal.SIGTERM)
                    p.wait(5)
                except Exception:
                    try:
                        os.killpg(p.pid, signal.SIGKILL)
                    except Exception:
                        pass
    rp = d / f"{mode}.report.json"
    rep = json.loads(rp.read_text()) if rp.exists() else {"checks": [{"name": "godot wrote a report", "pass": False, "detail": {"log": str(d / "godot.log")}}]}
    if mode == "main" and (d / "camera.json").exists():
        rep["checks"] += published_checks(d, json.loads((d / "camera.json").read_text()))
    return rep


def published_checks(d: Path, cam: dict) -> list:
    """What Godot published, as daemon B delivered it to the bot."""
    recs = [json.loads(l) for l in (d / "watch.jsonl").read_text().splitlines() if l.strip()] if (d / "watch.jsonl").exists() else []
    t_move = cam["t_move_unix"] * 1000
    out = []

    def near(rec, pose):
        dp = max(abs(rec["head"]["pos"][i] - pose["pos"][i]) for i in range(3))
        return dp, qdot(rec["head"]["rot"], pose["rot"])

    before = [r for r in recs if r["t_unix_ms"] < t_move - 300]
    after = [r for r in recs if r["t_unix_ms"] > t_move + 700]
    ok_a = bool(before) and all(near(r, cam["a"])[0] < 0.002 and near(r, cam["a"])[1] > 0.9999 for r in before)
    ok_b = bool(after) and all(near(r, cam["b"])[0] < 0.002 and near(r, cam["b"])[1] > 0.9999 for r in after)
    out.append({"name": "published head equals the desktop camera (position within 2 mm, rotation)", "pass": ok_a,
                "detail": {"records": len(before), "example": before[-1]["head"] if before else None, "camera": cam["a"]}})
    out.append({"name": "published head follows the camera when it moves (within 0.7 s)", "pass": ok_b,
                "detail": {"records": len(after), "example": after[-1]["head"] if after else None, "camera": cam["b"]}})
    flags_ok = bool(recs) and all(r["flags"] == (4 if cam["xr"] else 0) for r in recs)
    out.append({"name": "desktop player publishes no tracked hands and is not flagged VR", "pass": flags_ok,
                "detail": {"flags": sorted({r["flags"] for r in recs})}})
    if len(recs) > 20:
        span = (recs[-1]["t_unix_ms"] - recs[0]["t_unix_ms"]) / 1000
        rate = (len(recs) - 1) / span
        out.append({"name": "delivered at 15-25 Hz", "pass": 15 <= rate <= 25, "detail": {"hz": round(rate, 1)}})
    else:
        out.append({"name": "delivered at 15-25 Hz", "pass": False, "detail": {"records": len(recs)}})
    return out


reports = {}
for mode in ([a.only] if a.only else ["main", "stop", "kill"]):
    reports[mode] = run_scenario(mode)
bad = 0
for mode, r in reports.items():
    for c in r["checks"]:
        print(f"  {'PASS' if c['pass'] else 'FAIL'} [{mode}] {c['name']}  {json.dumps(c['detail']) if not c['pass'] else ''}")
        bad += 0 if c["pass"] else 1
print(f"logs: {OUT}")
if bad:
    print(f"POSE_FAILED ({bad})" + (f" (expected: fault {a.fault})" if a.fault else ""))
    sys.exit(1)
print("POSE_PASSED")
