#!/usr/bin/env python3
"""A1 oracle (Gate A check 10): kill -9 the Godot client mid-call; the call must go on.

Two room daemons on loopback, headless. Daemon A is started by a headless Godot client
(role reattach1), which hosts, names itself "Survivor" and unmutes; its session starts the
voice client with a tone source into a null sink (PRIM_SESSION_VOICE_TEST). Daemon B joins
A as the friend. Then:

  1. record A's voice process, voice incarnation (from A's rt holders), voice start count,
     member list and rt.sent
  2. kill -9 Godot
  3. assert the session, the room daemon and the voice process are the same, voice
     incarnation and start count are unchanged, both members are still in the room, and
     rt.sent keeps growing (the stream never stopped)
  4. start Godot again (role reattach2): same member id, same name, same mute state
  5. assert voice again unchanged (no restart on adoption)
  6. toggle deafen and mic through the session (`prim ctl session ...`): every command
     succeeds, each intermediate state is what was asked for, and the voice process, start
     count and incarnation stay the same (VC: changes happen in place); the peer's daemon
     sees a voice-flag update for each toggle
  7. kill -9 the session (voice survives), start a new one (it adopts voice): a toggle through
     the adopted voice keeps the same process and incarnation
  8. kill -9 the voice process with no client attached: the session relaunches it MUTED

Faults, each of which must turn the run red (--fault NAME):
  restart-on-attach   the relaunched client cycles voice off and on at startup, which
                      starts a new voice process: step 5 must catch the new incarnation
                      (a mute/deafen toggle no longer restarts voice since round VC, so it
                      cannot be the fault; step 6 below is its positive check)
  restart-on-toggle   the step-6 deafen toggle is replaced by a voice off/on cycle (what a
                      restart-based mute would do): step 6 must catch the new voice
  quit-on-close       the first client ends the session when it dies (simulated by
                      `prim ctl session quit` at the moment of the kill): step 3 must catch it

Tests use private tone/null audio only; nothing touches real microphones or speakers.
Pass an output directory as the first positional argument (default /tmp/prim-session-survive).
"""
import argparse, json, os, shutil, signal, socket, subprocess, sys, time
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument("out", nargs="?", default="/tmp/prim-session-survive")
ap.add_argument("--fault", default="", choices=["", "restart-on-attach", "quit-on-close", "restart-on-toggle"])
args = ap.parse_args()

ROOT = Path(__file__).resolve().parents[1]
OUT = Path(args.out)
shutil.rmtree(OUT, ignore_errors=True)
for d in ("a", "b", "godot"):
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
if not godot or not prim.exists():
    sys.exit("need target/debug/prim, target/debug/libprim_native.so staged in client/bin/linux, and a godot engine")


def free_port():
    s = socket.socket(); s.bind(("127.0.0.1", 0)); p = s.getsockname()[1]; s.close(); return p


if not (ROOT / "client/.godot/extension_list.cfg").exists():
    subprocess.run([godot, "--headless", "--path", str(ROOT / "client"), "--import"], capture_output=True, timeout=300)

pa = free_port()
sock_a, sock_b = OUT / "a/room.sock", OUT / "b/room.sock"
mine = []  # pids this run started (never kill by name)
checks = []


def check(name, ok, detail=None):
    checks.append((name, bool(ok), detail))
    print(f"  {'PASS' if ok else 'FAIL'} {name}  {json.dumps(detail) if detail is not None else ''}")
    return ok


def ctl(sock, *a):
    r = subprocess.run([str(prim), "ctl", "--socket", str(sock), *a], capture_output=True, text=True, timeout=30)
    return r.returncode, r.stdout, r.stderr


def jctl(sock, *a):
    rc, out, err = ctl(sock, *a)
    try:
        return json.loads(out)
    except ValueError:
        return {"_error": err or out, "_rc": rc}


def snapshot():
    sess = jctl(sock_a, "session", "status")
    st = jctl(sock_a, "status", "--json")
    rt = st.get("rt") or {}
    voice_h = [h for h in rt.get("holders", []) if h.get("role") == "voice"]
    return {
        "session_pid": sess.get("pid"),
        "voice": {k: (sess.get("voice") or {}).get(k) for k in ("running", "pid", "starts", "mic", "deaf", "state")},
        "incarnation": [(h["session"], h["incarnation"]) for h in voice_h],
        "members": sorted(m.get("key", "") for m in st.get("members", [])),
        "sent": rt.get("sent"),
        "room": sess.get("room"),
        "_err": sess.get("_error") or st.get("_error"),
    }


def pid_alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except (OSError, TypeError):
        return False


def start_godot(role, log, extra_env=None):
    env = os.environ | {
        "XDG_DATA_HOME": str(OUT / "godot/data"),
        "XDG_STATE_HOME": str(OUT / "a/state"),
        "XDG_RUNTIME_DIR": str(OUT / "a/run"),
        "PRIM_SOCKET": str(sock_a),
        "PRIM_BIN": str(prim),
        "PRIM_SESSION_VOICE_TEST": "1",
        "PRIM_DAEMON_ARGS": f"--relay none --bind 127.0.0.1:{pa} --state-dir {OUT}/a/room-state",
        "ROOM_TEST_FAULT": args.fault if args.fault == "restart-on-attach" else "",
    } | (extra_env or {})
    cmd = [godot, "--headless", "--path", str(ROOT / "client"), "--xr-mode", "off", "--", "--desktop", "--role", role,
           "--test-dir", str(OUT), "--socket", str(sock_a), "--name", "Godot-default"]
    p = subprocess.Popen(cmd, env=env, stdout=open(OUT / log, "w"), stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, start_new_session=True)
    mine.append(p.pid)
    return p


def wait_file(path, secs, proc=None):
    end = time.time() + secs
    while time.time() < end:
        if path.exists():
            time.sleep(0.2)
            return json.loads(path.read_text())
        if proc is not None and proc.poll() is not None:
            return None
        time.sleep(0.1)
    return None


peer = None
g1 = g2 = None
try:
    print(f"A: {sock_a}   B: {sock_b}   fault: {args.fault or 'none'}")
    g1 = start_godot("reattach1", "godot1.log")
    info = wait_file(OUT / "reattach1.json", 120, g1)
    if not check("first client hosted, named itself and unmuted", info is not None, None if info else "see godot1.log"):
        raise SystemExit
    # The friend: a second daemon joins A's room.
    penv = os.environ | {"XDG_STATE_HOME": str(OUT / "b/state")}
    peer = subprocess.Popen(
        [str(prim), "room", "join", info["token"], "--socket", str(sock_b), "--rt-socket", str(OUT / "b/rt.sock"),
         "--relay", "none", "--bind", "127.0.0.1:0", "--state-dir", str(OUT / "b/room-state"),
         "--peer-addr", f"{info['key']}=127.0.0.1:{pa}"],
        env=penv, stdout=open(OUT / "peer.log", "w"), stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, start_new_session=True)
    mine.append(peer.pid)
    end = time.time() + 60
    before = snapshot()
    while time.time() < end and not (len(before["members"]) >= 2 and before["incarnation"]):
        time.sleep(0.5)
        before = snapshot()
    check("before: two members, voice holds the lease, mic on",
          len(before["members"]) == 2 and before["incarnation"] and before["voice"]["mic"] and before["voice"]["running"], before)
    time.sleep(2.0)
    before = snapshot()

    # --- the property: kill the client mid-call -------------------------------------------
    os.killpg(g1.pid, signal.SIGKILL)  # Godot's whole process group, as closing a terminal would
    g1.wait()
    if args.fault == "quit-on-close":
        ctl(sock_a, "session", "quit")
    time.sleep(4.0)
    mid = snapshot()
    check("after kill -9: session is the same process", mid["session_pid"] == before["session_pid"] and pid_alive(mid["session_pid"]), {"before": before["session_pid"], "after": mid["session_pid"]})
    check("after kill -9: voice is the same process and was not restarted",
          mid["voice"]["pid"] == before["voice"]["pid"] and mid["voice"]["starts"] == before["voice"]["starts"] and pid_alive(mid["voice"]["pid"]),
          {"before": before["voice"], "after": mid["voice"]})
    check("after kill -9: same voice incarnation on the lane", mid["incarnation"] == before["incarnation"] and bool(mid["incarnation"]), {"before": before["incarnation"], "after": mid["incarnation"]})
    check("after kill -9: both members are still in the room", mid["members"] == before["members"] and len(mid["members"]) == 2, mid["members"])
    check("after kill -9: the stream kept flowing (rt.sent grew)", (mid["sent"] or 0) > (before["sent"] or 0), {"before": before["sent"], "after": mid["sent"]})

    # --- reopen -----------------------------------------------------------------------------
    g2 = start_godot("reattach2", "godot2.log", {"ROOM_TEST_EXPECT_ME": info["key"], "ROOM_TEST_EXPECT_NAME": info["name"]})
    again = wait_file(OUT / "reattach2.json", 90, g2)
    check("relaunched client reported", again is not None, None if again else "see godot2.log")
    if again:
        check("relaunch: same member id and name", again["key"] == info["key"] and again["name"] == info["name"], {"key": again["key"][:8], "name": again["name"]})
        check("relaunch: same mute state", again["voice"].get("mic") == before["voice"]["mic"] and again["voice"].get("deaf") == before["voice"]["deaf"], again["voice"])
    after = snapshot()
    check("relaunch: voice is still the same process, same start count",
          after["voice"]["pid"] == before["voice"]["pid"] and after["voice"]["starts"] == before["voice"]["starts"], {"before": before["voice"], "after": after["voice"]})
    check("relaunch: same voice incarnation", after["incarnation"] == before["incarnation"] and bool(after["incarnation"]), {"before": before["incarnation"], "after": after["incarnation"]})
    check("relaunch: both members still in the room", after["members"] == before["members"], after["members"])
    # Step 6: toggles never restart voice. Each command must succeed and each intermediate
    # state must be the one asked for (not just the same state before and after).
    def peer_sees_update():
        # B's daemon: wait for the next update of A's voice key (started before the toggle).
        return subprocess.Popen([str(prim), "ctl", "--socket", str(sock_b), "sub", f"member/{info['key']}/voice", "--count", "1", "--timeout", "6"],
                                stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)

    steps = [(("deafen", "on"), {"deaf": True, "mic": True}), (("deafen", "off"), {"deaf": False, "mic": True}),
             (("mic", "off"), {"deaf": False, "mic": False}), (("mic", "on"), {"deaf": False, "mic": True})]
    if args.fault == "restart-on-toggle":
        steps[0] = (("voice", "cycle"), {"deaf": True, "mic": True})
    for op, want in steps:
        sub = peer_sees_update()
        time.sleep(0.4)
        if op == ("voice", "cycle"):
            rcs = [ctl(sock_a, "session", "voice", "off")[0], ctl(sock_a, "session", "voice", "on")[0], ctl(sock_a, "session", "mic", "on")[0], ctl(sock_a, "session", "deafen", "on")[0]]
            rc = max(rcs)
        else:
            rc = ctl(sock_a, "session", *op)[0]
        time.sleep(1.5)
        snap = snapshot()
        early = sub.poll() is not None  # a `sub` that ended before the toggle proves nothing
        out = sub.communicate(timeout=15)[0]
        ok = (rc == 0 and snap["voice"]["deaf"] == want["deaf"] and snap["voice"]["mic"] == want["mic"]
              and snap["voice"]["pid"] == before["voice"]["pid"] and snap["voice"]["starts"] == before["voice"]["starts"]
              and snap["incarnation"] == before["incarnation"] and bool(snap["incarnation"]))
        check(f"toggle {' '.join(op)}: ok, state as asked, same voice process/starts/incarnation", ok, {"rc": rc, "want": want, "voice": snap["voice"], "incarnation": snap["incarnation"]})
        check(f"toggle {' '.join(op)}: the peer's daemon saw a voice-flag update", sub.returncode == 0 and bool(out.strip()) and not early, {"rc": sub.returncode, "early": early, "out": out.strip()[:120]})
    # Leave mic on, deafen off for the rest.
    ctl(sock_a, "session", "mic", "on"); ctl(sock_a, "session", "deafen", "off")
    time.sleep(1.0)
    if g2 is not None and g2.poll() is None:
        g2.terminate()
    voices_seen = [before["voice"]["pid"]]

    # Step 7: kill -9 the session; a new one adopts the running voice; a toggle keeps it.
    sess0 = jctl(sock_a, "session", "status")
    voice_pid = sess0["voice"]["pid"]
    inc0 = snapshot()["incarnation"]
    os.kill(sess0["pid"], signal.SIGKILL)
    time.sleep(1.0)
    senv = os.environ | {"XDG_STATE_HOME": str(OUT / "a/state"), "XDG_RUNTIME_DIR": str(OUT / "a/run"), "PRIM_SOCKET": str(sock_a),
                         "PRIM_BIN": str(prim), "PRIM_SESSION_VOICE_TEST": "1"}
    r = subprocess.run([str(prim), "ctl", "--socket", str(sock_a), "session", "ensure"], env=senv, capture_output=True, text=True, timeout=60)
    time.sleep(2.0)
    adopted = snapshot()
    check("adopted: a new session took over the running voice (same pid, same incarnation, mic on)",
          r.returncode == 0 and adopted["session_pid"] != sess0["pid"] and adopted["voice"]["pid"] == voice_pid
          and adopted["incarnation"] == inc0 and adopted["voice"]["mic"], {"rc": r.returncode, "before": sess0["voice"], "after": adopted["voice"]})
    rcs = [ctl(sock_a, "session", "mic", "off")[0]]
    time.sleep(1.0)
    mid2 = snapshot()
    rcs.append(ctl(sock_a, "session", "mic", "on")[0])
    time.sleep(1.0)
    adopted2 = snapshot()
    check("adopted: mic off then on through the adopted voice keeps the same process and incarnation",
          max(rcs) == 0 and mid2["voice"]["mic"] is False and adopted2["voice"]["mic"] is True
          and adopted2["voice"]["pid"] == voice_pid and adopted2["incarnation"] == inc0, {"rcs": rcs, "mid": mid2["voice"], "after": adopted2["voice"]})

    # Step 8: kill -9 the voice with no client attached: it must come back muted.
    os.kill(voice_pid, signal.SIGKILL)
    voices_seen.append(voice_pid)
    back = None
    for _ in range(120):
        time.sleep(0.25)
        snap = snapshot()
        if snap["voice"]["running"] and snap["voice"]["pid"] not in (None, voice_pid):
            back = snap
            break
    check("relaunch after kill -9 of voice: comes back, and MUTED (no client attached)",
          back is not None and back["voice"]["mic"] is False, back and back["voice"])
    if back:
        voices_seen.append(back["voice"]["pid"])
        mine.append(back["voice"]["pid"])
    # Window close with no tray (tray_live is false until round T1): leave + quit, so no
    # live microphone is left with nothing visible. With the override it keeps everything.
    for keep in ("1", ""):
        (OUT / "closer.json").unlink(missing_ok=True)
        gc = start_godot("closer", f"closer{keep}.log", {"PRIM_CLOSE_KEEPS_SESSION": keep})
        got = wait_file(OUT / "closer.json", 60, gc)
        time.sleep(6.0)
        s = jctl(sock_a, "session", "status")
        up = "pid" in s
        if keep:
            check("window close with PRIM_CLOSE_KEEPS_SESSION=1: session, room and voice keep running",
                  up and s.get("room") == "up" and (s.get("voice") or {}).get("running"), s.get("room"))
            gc.terminate()
        else:
            check("tray_live is false and window close leaves the room and quits everything",
                  got is not None and got["tray_live"] is False and not up, {"tray_live": got and got["tray_live"], "session_up": up})
            check("no voice process is left after a close with no tray",
                  not any(pid_alive(p) for p in voices_seen), voices_seen)
except SystemExit:
    pass
finally:
    # End only what we started: the session (and through it room + voice), then stragglers by pid.
    try:
        ctl(sock_a, "session", "quit")
    except Exception:
        pass
    for p in (g1, g2, peer):
        if p is not None and p.poll() is None:
            p.terminate()
            try:
                p.wait(5)
            except subprocess.TimeoutExpired:
                p.kill()
    time.sleep(1.0)
    for pid in mine:
        if pid_alive(pid):
            try:
                os.kill(pid, signal.SIGTERM)
            except OSError:
                pass

bad = [c for c in checks if not c[1]]
print(f"logs: {OUT}")
if args.fault:
    print(("SESSION_SURVIVE_FAULT_CAUGHT" if bad else "SESSION_SURVIVE_FAULT_MISSED") + f" ({args.fault}: {len(bad)} failing)")
    sys.exit(0 if bad else 1)
print("SESSION_SURVIVE_PASSED" if not bad and checks else f"SESSION_SURVIVE_FAILED ({len(bad)})")
sys.exit(1 if bad or not checks else 0)
