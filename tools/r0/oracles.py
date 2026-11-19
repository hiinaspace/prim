#!/usr/bin/env python3
"""R0 oracle harness (docs/room/VOICE.md, "Oracles (R0)"). Stdlib only.

    uv run --no-project python tools/r0/oracles.py
        <baseline|bulk|listener|faults|stale|scenes|loss|reorder|drift|quantum|all>
        [--prim PATH] [--out DIR] [--scale F] [--clips DIR] [--godot DIR]

Runs on the Linux test host. Everything audible goes to PipeWire null sinks this script
creates; the listener's ALSA default is redirected with a private HOME/.asoundrc whose
playback/capture nodes are our sink and our virtual source, with node.dont-fallback so a
missing node can never fall back to a real device. A silent link proof (pw-link -l) runs
before any talker starts, and a watchdog kills the listener if it ever links elsewhere.
Writes <run>/summary.json and <run>/summary.md.
"""
import argparse, array, atexit, base64, hashlib, json, math, os, platform, queue, shutil, signal
import socket, struct, subprocess, sys, threading, time, wave, re, datetime

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
TAG = "Signature: 8a477f597d28d172789f06886806bc55\n"
PFX = "prim-r0-"
RATE = 48000
SEATS = [-150, -100, -50, 0, 50, 100, 150]
SKEWS = [0, 500, -500, 0, 250, -250, 0]
CLIP_ORDER = [0, 1, 2, 3, 0, 1, 2]  # per-talker clip index (default i % 4)
LISTENER_EXTRA = []  # extra voice args for the listener (the idle A/B turns the skip off)
LISTENER_PRIM = None  # --listener-prim: run only the listener (node L) with another build (old-receiver proofs)
LISTENER_ENV = {}  # extra environment for the listener's ALSA stream (the quantum scenario)
DROP_EVERY = 0  # --drop-every: test-only packet-loss fault on the talkers (proves oracle 1b can fail)
GODOT_DEF = os.path.expanduser("~/code/prim-hrtf/.cache/bench/r2a-4")
mono = time.monotonic
T0 = mono()


def log(*a):
    print("[%7.2f]" % (mono() - T0), *a, flush=True)


# ---------------------------------------------------------------- process registry
PROCS = []
CLEANUPS = []


def cleanup_all():
    for p in list(PROCS):
        try:
            p.kill_group()
        except Exception:
            pass
    for f in reversed(CLEANUPS):
        try:
            f()
        except Exception:
            pass
    CLEANUPS.clear()


atexit.register(cleanup_all)
for _s in (signal.SIGTERM, signal.SIGHUP):
    signal.signal(_s, lambda *a: sys.exit(130))


class Proc:
    def __init__(self, name, argv, logpath, env=None, stdout=None):
        self.name, self.argv = name, argv
        self.logf = open(logpath, "ab")
        self.p = subprocess.Popen(argv, stdout=stdout or self.logf, stderr=self.logf,
                                  stdin=subprocess.DEVNULL, env=env, start_new_session=True)
        PROCS.append(self)

    @property
    def pid(self):
        return self.p.pid

    def alive(self):
        return self.p.poll() is None

    def sig(self, s):
        if self.alive():
            os.kill(self.p.pid, s)

    def kill9(self):
        self.sig(signal.SIGKILL)
        self.p.wait()

    def term(self, wait=1.5):
        self.sig(signal.SIGTERM)
        try:
            self.p.wait(wait)
        except subprocess.TimeoutExpired:
            self.kill9()

    def kill_group(self):
        try:
            os.killpg(self.p.pid, signal.SIGCONT)
            os.killpg(self.p.pid, signal.SIGKILL)
        except Exception:
            pass
        try:
            self.p.wait(2)
        except Exception:
            pass


def sh(cmd, check=True, timeout=30):
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    if check and r.returncode:
        raise RuntimeError("%s: %s" % (cmd, r.stderr.strip()))
    return r.stdout


# ---------------------------------------------------------------- PipeWire
class Audio:
    """Null sinks, the virtual mic, link proof and a watchdog. Never touches defaults."""

    def __init__(self):
        self.mods = {}
        self.mic_id = None
        self.default_before = self.default_sink()
        self.violation = None
        self.owner = None  # callable -> pids of the prim processes this run started
        self.stop_wd = threading.Event()
        CLEANUPS.append(self.close)

    @staticmethod
    def default_sink():
        for l in sh(["pactl", "info"]).splitlines():
            if l.startswith("Default Sink:") or l.startswith("Default Source:"):
                pass
        d = {}
        for l in sh(["pactl", "info"]).splitlines():
            if l.startswith("Default Sink:"):
                d["sink"] = l.split(": ", 1)[1]
            if l.startswith("Default Source:"):
                d["source"] = l.split(": ", 1)[1]
        return d

    def add_sink(self, name):
        self.mods[name] = sh(["pactl", "load-module", "module-null-sink", "sink_name=" + name]).strip()
        return name

    def rm_sink(self, name):
        m = self.mods.pop(name, None)
        if m:
            sh(["pactl", "unload-module", m], check=False)

    def add_mic(self, name):
        sh(["pw-cli", "create-node", "adapter",
            "{ factory.name=support.null-audio-sink node.name=%s media.class=Audio/Source/Virtual "
            "audio.position=[FL FR] object.linger=true }" % name])
        self.mic_id = name

    def rm_mic(self):
        if not self.mic_id:
            return
        import json as j
        for o in j.loads(sh(["pw-dump"])):
            if o.get("info", {}).get("props", {}).get("node.name") == self.mic_id:
                sh(["pw-cli", "destroy", str(o["id"])], check=False)
        self.mic_id = None

    def close(self):
        self.stop_wd.set()
        for n in list(self.mods):
            self.rm_sink(n)
        self.rm_mic()

    @staticmethod
    def links():
        """{node_port: [peer node names]} from pw-link -l."""
        out, cur = {}, None
        for l in sh(["pw-link", "-l"]).splitlines():
            if not l.startswith(" "):
                cur = l.strip()
                out[cur] = []
            elif cur and ("|->" in l or "|<-" in l):
                out[cur].append(l.split(None, 1)[1].split(":")[0])
        return out

    def stream_targets(self, pids_names=("alsa_playback.prim", "alsa_capture.prim"), owned=False):
        """Peer node names that prim's ALSA streams link to. All streams of that name by default; with
        owned=True only those of the prim processes this run started (another session's prim on this
        host has streams of the same name, which must neither trip the watchdog nor hide ours)."""
        if owned and self.owner:
            return self.owned_targets(pids_names)
        res = {}
        for port, peers in self.links().items():
            node = port.split(":")[0]
            if node in pids_names:
                res.setdefault(node, set()).update(peers)
        return res

    def owned_targets(self, pids_names):
        """Links of the ALSA streams whose client process is one of ours (pw-dump: node -> client -> pid)."""
        import json as j
        objs = j.loads(sh(["pw-dump"]))
        mine = set(self.owner())
        # a client with no pid property is treated as ours: when in doubt, the watchdog fires
        own = lambda cid: pid_of.get(cid) in mine or pid_of.get(cid) is None
        pid_of = {o["id"]: o["info"]["props"].get("application.process.id") for o in objs
                  if o.get("type") == "PipeWire:Interface:Client" and o.get("info", {}).get("props")}
        nodes = {o["id"]: o["info"]["props"] for o in objs if o.get("type") == "PipeWire:Interface:Node" and o.get("info", {}).get("props")}
        res = {}
        for nid, pr in nodes.items():
            if pr.get("node.name") in pids_names and own(pr.get("client.id")):
                res.setdefault(pr["node.name"], set())
        for o in objs:
            if o.get("type") == "PipeWire:Interface:Link":
                i = o.get("info", {})
                a, b = i.get("output-node-id"), i.get("input-node-id")
                for me, peer in ((a, b), (b, a)):
                    pr = nodes.get(me)
                    if pr and pr.get("node.name") in res and own(pr.get("client.id")) and peer in nodes:
                        res[pr["node.name"]].add(nodes[peer].get("node.name", "?"))
        return res

    def check_links(self):
        bad = {n: sorted(p for p in ps if not p.startswith(PFX)) for n, ps in self.stream_targets().items()}
        bad = {n: p for n, p in bad.items() if p}
        if bad and self.owner:  # confirm it is one of ours before killing the run
            bad = {n: sorted(p for p in ps if not p.startswith(PFX)) for n, ps in self.stream_targets(owned=True).items()}
            bad = {n: p for n, p in bad.items() if p}
        return bad

    def prove_silent_links(self, expect_sink, tries=150):
        """Listener running silent: its playback links only to expect_sink, capture only to our mic."""
        for _ in range(tries):
            t = self.stream_targets(owned=True)
            if "alsa_playback.prim" in t and "alsa_capture.prim" in t:
                bad = self.check_links()
                if bad:
                    raise RuntimeError("LINK TO NON-TEST NODE: %s" % bad)
                if t["alsa_playback.prim"] == {expect_sink}:
                    return {k: sorted(v) for k, v in t.items()}
            time.sleep(0.1)
        raise RuntimeError("could not prove listener links (got %s)" % self.stream_targets())

    def start_watchdog(self, on_violation):
        def run():
            while not self.stop_wd.wait(0.25):
                try:
                    bad = self.check_links()
                except Exception:
                    continue
                if bad:
                    self.violation = bad
                    on_violation()
                    return
        threading.Thread(target=run, daemon=True).start()


class Recorder:
    """pw-record of a null sink monitor, raw f32 stereo, with chunk arrival times."""

    def __init__(self, sink, logdir, latency="10ms"):
        self.sink = sink
        self.buf = array.array("f")
        self.times = []  # (t_arrival, frames_so_far)
        self.proc = Proc("rec-" + sink, ["pw-record", "--target", sink, "-P",
                         "{ stream.capture.sink=true node.dont-fallback=true }", "--rate", "48000", "--channels", "2",
                         "--format", "f32", "--latency", latency, "-"], os.path.join(logdir, "rec-%s.log" % sink),
                         stdout=subprocess.PIPE)
        self.lock = threading.Lock()
        self.th = threading.Thread(target=self._rd, daemon=True)
        self.th.start()
        self.rem = b""

    def _rd(self):
        f = self.proc.p.stdout
        while True:
            b = f.read1(65536)
            if not b:
                break
            t = mono()
            b = self.rem + b
            n = len(b) // 8 * 8
            self.rem = b[n:]
            a = array.array("f")
            a.frombytes(b[:n])
            with self.lock:
                self.buf.extend(a)
                self.times.append((t, len(self.buf) // 2))

    def stop(self):
        self.proc.term(2)
        self.th.join(2)

    def clock(self):
        """Estimated monotonic time of frame 0: min over chunks of (arrival - frames/RATE)."""
        with self.lock:
            return min((t - fr / RATE for t, fr in self.times), default=None)

    def mono_signal(self, t_from=0.0):
        with self.lock:
            b = self.buf
            n = len(b) // 2
            return [(b[2 * i] + b[2 * i + 1]) * 0.5 for i in range(int(t_from * RATE), n)]

    def slice_st(self, t_start, t_end):
        c = self.clock()
        i0, i1 = max(0, int((t_start - c) * RATE)), int((t_end - c) * RATE)
        with self.lock:
            b = self.buf
            i1 = min(i1, len(b) // 2)
            return list(b[2 * i0:2 * i1:2]), list(b[2 * i0 + 1:2 * i1:2])

    def slice_t(self, t_start, t_end):
        """Mono samples between two monotonic times and the index of the first."""
        c = self.clock()
        i0, i1 = max(0, int((t_start - c) * RATE)), int((t_end - c) * RATE)
        with self.lock:
            b = self.buf
            i1 = min(i1, len(b) // 2)
            return [(b[2 * i] + b[2 * i + 1]) * 0.5 for i in range(i0, i1)], i0, c


# ---------------------------------------------------------------- DSP (stdlib)
def tone_series(x, hz, win=2400, hop=480):
    """Amplitude of hz in sliding windows; list of (start_index, amp)."""
    if len(x) < win:
        return []
    w = 2 * math.pi * hz / RATE
    c = [math.cos(w * k) for k in range(win)]
    s = [math.sin(w * k) for k in range(win)]
    out = []
    for i in range(0, len(x) - win + 1, hop):
        seg = x[i:i + win]
        # phase advances with i; amplitude is phase independent
        re_, im = math.sumprod(seg, c), math.sumprod(seg, s)
        out.append((i, 2 * math.hypot(re_, im) / win))
    return out


def auto_hz(inc):
    return 300.0 + 20.0 * ((((inc * 0x9E3779B1) & 0xFFFFFFFF) >> 16) % 100)


def rms_db(x):
    if not x:
        return -200.0
    e = math.sumprod(x, x) / len(x)
    return 10 * math.log10(max(e, 1e-20))


def pct(sorted_v, p):
    if not sorted_v:
        return None
    r = max(1, math.ceil(p * len(sorted_v) / 100 - 1e-9))
    return sorted_v[min(r, len(sorted_v)) - 1]


def bucket_pct(counts, p):
    """Upper edge (us) of the log2 bucket holding percentile p. bucket i = [2^(i-1), 2^i)."""
    n = sum(counts)
    if n == 0:
        return None
    rank = max(1, math.ceil(p * n / 100 - 1e-9))
    acc = 0
    for i, c in enumerate(counts):
        acc += c
        if acc >= rank:
            return 0 if i == 0 else 2 ** i
    return None


def hist_row(h):
    """Voice-stats or daemon histogram -> {n,p50,p99,p99_9,max} in us."""
    if not h:
        return {}
    if "p50_us" in h:
        return {"n": h["count"], "p50": h["p50_us"], "p99": h["p99_us"], "p99_9": h["p999_us"], "max": h["max_us"]}
    return {"n": h["count"], "p50": h.get("p50"), "p99": h.get("p99"), "p99_9": h.get("p99_9"), "max": h.get("max")}


def rget(r, *names, default=None):
    """First present key: the timeline receiver's stat names, then the old NetEq ones."""
    for n in names:
        if r.get(n) is not None:
            return r[n]
    return default


def proc_cpu_s(pid):
    """utime + stime of a process in seconds, or None."""
    try:
        f = open("/proc/%d/stat" % pid).read().rsplit(")", 1)[1].split()
        return (int(f[11]) + int(f[12])) / os.sysconf("SC_CLK_TCK")
    except Exception:
        return None


MIXER_HISTS = ("mixer_block_us", "mixer_lock_us", "mixer_pull_us", "mixer_hrtf_us", "mixer_sum_us")


def mixer_rows(v):
    """Mixer block and per-stage histograms (per-block totals over all senders), in us."""
    h = v.get("histograms", {})
    return {k.replace("mixer_", "").replace("_us", ""): hist_row(h.get(k)) for k in MIXER_HISTS if h.get(k)}


def peer_means(statlog, key, t_from):
    """Mean buffer_ms / target_delay_ms of one peer over the stats polled after t_from."""
    rs = [s["peers"].get(key, {}).get("receiver", {}) for tt, s in statlog if tt >= t_from]
    out = {}
    for k in ("buffer_ms", "target_delay_ms"):
        vs = [r[k] for r in rs if r.get(k) is not None]
        out[k + "_mean"] = sum(vs) / len(vs) if vs else None
        out[k + "_max"] = max(vs) if vs else None
    return out


# ---------------------------------------------------------------- local API client
class Local:
    def __init__(self, path):
        self.s = socket.socket(socket.AF_UNIX)
        self.s.connect(path)
        self.req = 1
        self.waiters = {}
        self.lock = threading.Lock()
        self.wl = threading.Lock()
        self.alive = True
        self.on_event = None
        threading.Thread(target=self._rd, daemon=True).start()

    def _rd(self):
        try:
            while True:
                h = self._exact(4)
                n = struct.unpack("<I", h)[0]
                ev = json.loads(self._exact(n))
                if ev.get("event") == "reply":
                    with self.lock:
                        w = self.waiters.pop(ev["req"], None)
                    if w:
                        w[1].append(ev["reply"])
                        w[0].set()
                elif self.on_event:
                    self.on_event(ev)
        except Exception:
            self.alive = False

    def _exact(self, n):
        b = b""
        while len(b) < n:
            c = self.s.recv(n - len(b))
            if not c:
                raise EOFError
            b += c
        return b

    def send(self, obj):
        body = json.dumps(obj).encode()
        with self.wl:
            self.s.sendall(struct.pack("<I", len(body)) + body)

    def call(self, op, timeout=10, **kw):
        with self.lock:
            r = self.req
            self.req += 1
            w = (threading.Event(), [])
            self.waiters[r] = w
        self.send(dict(op=op, req=r, **kw))
        if not w[0].wait(timeout):
            with self.lock:
                self.waiters.pop(r, None)
            raise TimeoutError(op)
        return w[1][0]

    def status(self, timeout=5):
        return self.call("status", timeout)["status"]

    def close(self):
        try:
            self.s.close()
        except Exception:
            pass


def b64(b):
    return base64.b64encode(b).decode()


# ---------------------------------------------------------------- nodes and cluster
class Node:
    def __init__(self, cl, name):
        self.cl, self.name = cl, name
        self.dir = os.path.join(cl.rt_dir, name)
        os.makedirs(self.dir, exist_ok=True)
        self.state = os.path.join(self.dir, "state")
        self.sock = os.path.join(self.dir, "room.sock")
        self.rt = os.path.join(self.dir, "rt.sock")
        self.stats = os.path.join(cl.logdir, name + ".stats.json")
        self.daemon = None
        self.voice = None
        self.key = None
        self.addr = None
        self.local = None
        self.vgen = 0
        self.statlog = []  # voice stats history (t, json)

    def start_daemon(self, token=None, peer=None, clock_offset=0):
        argv = [self.cl.prim, "room"]
        argv += ["join", token] if token else ["found"]
        argv += ["--state-dir", self.state, "--socket", self.sock, "--rt-socket", self.rt,
                 "--relay", "none", "--bind", "127.0.0.1:0"]
        if peer:
            argv += ["--peer-addr", peer]
        if clock_offset:
            argv += ["--rt-clock-offset-us", str(clock_offset)]
        lp = os.path.join(self.cl.logdir, self.name + ".room.log")
        self.daemon = Proc(self.name + "-room", argv, lp)
        for _ in range(300):
            txt = open(lp, errors="replace").read()
            if re.search(r"^ready$", txt, re.M):
                break
            if not self.daemon.alive():
                raise RuntimeError("daemon %s exited: %s" % (self.name, txt[-400:]))
            time.sleep(0.1)
        else:
            raise RuntimeError("daemon %s not ready" % self.name)
        m = re.search(r"^member (\w+)", txt, re.M)
        self.key = m.group(1)
        a = re.search(r"^listen (127\.\S+)", txt, re.M)
        self.addr = a.group(1) if a else None
        self.local = Local(self.sock)

    def voice_argv(self, capture, output, skew=0, extra=()):
        a = [LISTENER_PRIM if (LISTENER_PRIM and self.name == "L") else self.cl.prim, "voice", "--rt-socket", self.rt, "--capture", capture, "--output", output,
             "--stats", self.stats]
        if skew:
            a += ["--skew-ppm", str(skew)]
        return a + list(extra)

    def start_voice(self, capture, output, skew=0, extra=(), env=None, tag=""):
        self.vgen += 1
        lp = os.path.join(self.cl.logdir, "%s.voice%d%s.log" % (self.name, self.vgen, tag))
        try:
            os.unlink(self.stats)
        except OSError:
            pass
        self.voice = Proc(self.name + "-voice", self.voice_argv(capture, output, skew, extra), lp, env=env)
        self.voice_log = lp
        return self.voice

    def vstats(self):
        try:
            return json.load(open(self.stats))
        except Exception:
            return None

    def rt_status(self, timeout=5):
        return self.local.status(timeout)


# Daemon inbound-lane counters reported per scenario (age_media_tolerated is not a drop).
LANE_COUNTERS = ("age_media", "age_media_tolerated", "rate", "stream_rate")
CLUSTER_TALLIES = []


def lane_oracle(res, scen, tallies):
    """Per-scenario tally of the lane counters. `rate` and `stream_rate` must stay 0 everywhere
    (the fault scenarios restart processes, at most about once per second, so they are
    not throttled); `age_media` is expected only after a daemon stall of ours."""
    tot = {k: 0 for k in LANE_COUNTERS + ("stalls",)}
    per = {}
    for t in tallies:
        for node, c in t.items():
            for k, v in c.items():
                tot[k] += v
                per.setdefault(node, {}).setdefault(k, 0)
                per[node][k] += v
    res.data.setdefault(scen, {})["lane_counters"] = {"total": tot, "per_node": per}
    ok = tot["rate"] == 0 and tot["stream_rate"] == 0 and bool(tallies)
    if scen in ("baseline", "bulk", "listener"):
        ok = ok and tot["age_media"] == 0
    res.oracle(scen, "lane", "inbound lane: rate = stream_rate = 0" + ("; age_media = 0 (no stall of ours)" if scen in ("baseline", "bulk", "listener") else ""), ok,
               "age_media %d, age_media_tolerated %d, rate %d, stream_rate %d, daemon stalls detected %d" % (tot["age_media"], tot["age_media_tolerated"], tot["rate"], tot["stream_rate"], tot["stalls"]))


class Cluster:
    def __init__(self, run, prim, audio=None, name="c"):
        self.run, self.prim, self.audio = run, prim, audio
        self.logdir = os.path.join(run, "logs-" + name)
        os.makedirs(self.logdir, exist_ok=True)
        self.rt_dir = "/tmp/prim-r0-%d-%s" % (os.getpid(), name)
        os.makedirs(self.rt_dir, exist_ok=True)
        CLEANUPS.append(lambda: shutil.rmtree(self.rt_dir, ignore_errors=True))
        self.L = Node(self, "L")
        self.T = []
        if audio:
            audio.owner = lambda: [n.voice.pid for n in [self.L] + self.T if n.voice is not None and n.voice.pid]
        self.sinks = []
        self.recs = {}
        self.rec_list = []
        self.poll_stop = threading.Event()
        self.timeline = []  # (t, {name: status-rt})
        self.helpers = []
        self.home = None

    def start_daemons(self, n, l_offset=0):
        self.L.start_daemon(clock_offset=l_offset)
        for i in range(n):
            t = Node(self, "T%d" % (i + 1))
            tok = self.L.local.call("create_invite", expiry_in_ms=600000)["token"]
            t.start_daemon(token=tok, peer="%s=%s" % (self.L.key, self.L.addr))
            self.T.append(t)
        for t in self.T:
            for _ in range(300):
                s = t.rt_status()
                if any(r["peer"] == self.L.key for r in s["routes"]):
                    break
                time.sleep(0.1)
            else:
                raise RuntimeError("%s never connected to L" % t.name)
        log("daemons up: L + %d talkers" % n)

    # ---- listener on the null sinks
    def alsa_home(self, sink):
        self.home = os.path.join(self.rt_dir, "home")
        os.makedirs(self.home, exist_ok=True)
        self.set_sink(sink)

    def set_sink(self, sink):
        with open(os.path.join(self.home, ".asoundrc"), "w") as f:
            f.write('pcm.!default { type pipewire playback_node "%s" capture_node "%sMIC" }\n' % (sink, PFX))

    def listener_env(self):
        e = dict(os.environ)
        e["HOME"] = self.home
        e["PIPEWIRE_ALSA"] = "{ node.dont-fallback=true }"
        e.update(LISTENER_ENV)
        return e

    def start_listener(self, output, tracker=True, seats=True, extra=(), device=False, quiet=()):
        L = self.L
        ex = list(extra)
        if seats:
            for i, t in enumerate(self.T):
                ex += ["--seat", "%s=%s" % (t.key[:8], SEATS[i % 7])]
        env = self.listener_env() if device else None
        for n in quiet:  # nothing audible may reach the listener while its links are proven
            n.daemon.sig(signal.SIGSTOP)
        L.start_voice("device" if device else "none", output, extra=["--stats-tone"] + ex + list(LISTENER_EXTRA), env=env)
        if device:
            time.sleep(0.5)
            if not L.voice.alive():
                raise RuntimeError("listener died: " + open(L.voice_log).read()[-400:])
            # silent proof: no talker voice has started, so nothing audible exists yet
            proof = self.audio.prove_silent_links(self.sinks[0])
            log("link proof (silent):", proof)
            self.link_proof = proof
            self.audio.start_watchdog(self.panic)
        for n in quiet:
            n.daemon.sig(signal.SIGCONT)
        for _ in range(100):
            if (L.vstats() or {}).get("self_key"):
                break
            time.sleep(0.1)
        if tracker:
            self.helpers.append(Proc("track", [self.prim, "voice", "track", "--rt-socket", L.rt, "--rate", "90",
                                 "--sweep", "60"], os.path.join(self.logdir, "track.log")))

    def panic(self):
        log("SAFETY: listener linked to a non-test node; killing")
        self.L.voice and self.L.voice.kill_group()

    def start_talkers(self, specs, which=None, extra=()):
        """specs: list of (capture, skew) per talker index; `extra` goes to every talker."""
        for i, t in enumerate(self.T):
            if which and t.name not in which:
                continue
            cap, skew = specs[i]
            t.start_voice(cap, "null", skew=skew, extra=list(extra) + (["--test-drop-every", str(DROP_EVERY)] if DROP_EVERY else []))

    def pollers(self, period=1.0):
        def run():
            while not self.poll_stop.wait(period):
                now = mono()
                row = {}
                for n in [self.L] + self.T:
                    try:
                        if n.local and n.local.alive and (n.daemon and n.daemon.alive()):
                            row[n.name] = n.local.status(2)
                    except Exception:
                        pass
                self.timeline.append((now, row))
                s = self.L.vstats()
                if s:
                    self.L.statlog.append((now, s))
                for t in self.T:
                    s = t.vstats()
                    if s:
                        t.statlog.append((now, s))
        th = threading.Thread(target=run, daemon=True)
        th.start()

    def wait_flowing(self, names=None, timeout=40):
        end = mono() + timeout
        names = names or [t.name for t in self.T]
        while mono() < end:
            s = self.L.vstats()
            if s:
                ok = all(s["peers"].get(t.key, {}).get("packets_accepted", 0) > 0 for t in self.T if t.name in names)
                if ok:
                    return True
            time.sleep(0.2)
        return False

    def tally(self):
        """Final inbound-lane counters of every daemon (last good status), for the limits oracle."""
        out = {}
        last = self.timeline[-1][1] if self.timeline else {}
        for n in [self.L] + self.T:
            st = None
            try:
                if n.local and n.local.alive and n.daemon and n.daemon.alive():
                    st = n.local.status(2)
            except Exception:
                pass
            st = st or last.get(n.name)
            if st:
                rt = st["rt"]
                d = rt["drops"]
                out[n.name] = {k: d.get(k, 0) for k in LANE_COUNTERS}
                out[n.name]["stalls"] = rt.get("stalls", 0)
        return out

    def stop(self):
        self.poll_stop.set()
        try:
            CLUSTER_TALLIES.append(self.tally())
        except Exception as e:
            log("tally failed:", repr(e))
        for h in self.helpers:
            h.term(2)
        for n in self.T + [self.L]:
            if n.voice:
                n.voice.term(1.5)
        for n in self.T + [self.L]:
            if n.local:
                n.local.close()
            if n.daemon:
                n.daemon.term(1.5)
        for r in self.rec_list:
            r.stop()
        if self.audio:
            for n in list(self.audio.mods):
                self.audio.rm_sink(n)
            self.audio.rm_mic()


# ---------------------------------------------------------------- env, oracles, results
class Results:
    def __init__(self, run):
        self.run = run
        self.items = []
        self.data = {}

    def oracle(self, scen, oid, name, ok, detail):
        self.items.append({"scenario": scen, "id": oid, "name": name,
                           "pass": (None if ok is None else bool(ok)), "detail": detail})
        log("%s %-5s %s %s" % (scen, {True: "PASS", False: "FAIL", None: "INFO"}[None if ok is None else bool(ok)], oid, detail if len(str(detail)) < 180 else str(detail)[:180]))

    def save(self, env=None):
        d = {"oracles": self.items, "data": self.data}
        if env:
            d["environment"] = env
        json.dump(d, open(os.path.join(self.run, "summary.json"), "w"), indent=1, default=str)
        md = ["# R0 oracle run", "", "run: `%s`" % self.run, "",
              "| scenario | oracle | result | numbers |", "|---|---|---|---|"]
        for it in self.items:
            r = {True: "PASS", False: "**FAIL**", None: "info"}[it["pass"]]
            md.append("| %s | %s %s | %s | %s |" % (it["scenario"], it["id"], it["name"], r,
                                                   str(it["detail"]).replace("|", "/").replace("\n", " ")))
        if env:
            md += ["", "## environment", "```", json.dumps(env, indent=1), "```"]
        open(os.path.join(self.run, "summary.md"), "w").write("\n".join(md) + "\n")


def environment(prim):
    e = {"kernel": platform.release(), "python": platform.python_version()}
    # The test host's tree is rsynced (no .git), so the commit comes from the sender:
    # PRIM_R0_COMMIT = `git rev-parse --short HEAD` plus "+dirty" if uncommitted, and
    # PRIM_R0_DIFF_SHA = sha256 of `git diff HEAD` there (see tools/r0/provenance.sh).
    # Without them, fall back to asking git in ROOT.
    env_commit = os.environ.get("PRIM_R0_COMMIT", "").strip()
    if env_commit:
        e["commit"] = env_commit.split("+")[0]
        e["dirty"] = env_commit.endswith("+dirty")
        e["commit_source"] = "PRIM_R0_COMMIT"
        if os.environ.get("PRIM_R0_DIFF_SHA"):
            e["diff_sha256"] = os.environ["PRIM_R0_DIFF_SHA"].strip()
    else:
        try:
            e["commit"] = sh(["git", "-C", ROOT, "rev-parse", "--short", "HEAD"], check=False).strip()
            e["dirty"] = bool(sh(["git", "-C", ROOT, "status", "--porcelain"], check=False).strip())
            e["commit_source"] = "git (tree on the test host; unreliable if rsynced)"
        except Exception:
            pass
    # the voice receiver is in the godot-network-audio submodule (voice-core): its commit, like the
    # prim commit, comes from the sender when the tree was rsynced (PRIM_R0_SUB)
    sub = os.environ.get("PRIM_R0_SUB", "").strip()
    if not sub:
        sub = sh(["git", "-C", os.path.join(ROOT, "dependencies/godot-network-audio"), "rev-parse", "--short", "HEAD"], check=False).strip()
    e["voice_core_commit"] = sub
    try:
        e["prim_sha256"] = hashlib.sha256(open(prim, "rb").read()).hexdigest()
    except OSError:
        pass
    e["pipewire"] = " ".join(sh(["pw-cli", "--version"], check=False).split()[-3:])
    try:
        md = sh(["pw-metadata", "-n", "settings"], check=False)
        e["pipewire_settings"] = {k: v for k, v in re.findall(r"key:'clock\.(\w[\w.-]*)' value:'([^']*)'", md)}
    except Exception:
        pass
    for l in open("/proc/cpuinfo"):
        if l.startswith("model name"):
            e["cpu"] = l.split(":", 1)[1].strip()
            break
    e["cpus"] = os.cpu_count()
    e["prim_binary"] = prim
    e["build_profile"] = "release" if "/release/" in prim else ("debug" if "/debug/" in prim else "unknown")
    return e


def keyname(cl, key):
    for t in cl.T:
        if t.key == key:
            return t.name
    return "L" if key == cl.L.key else key[:8]


def stress_cmd():
    p = sh(["nix-shell", "-p", "stress-ng", "--run", "command -v stress-ng"], timeout=300).strip()
    return p


def bulk_helpers(cl, run, stop_evt, counters):
    """Reliable-message flood T1 -> L, state-write flood on L, stress-ng."""
    L, T1 = cl.L, cl.T[0]
    # L drains channel 77 so deliveries complete
    drain = Local(L.sock)
    drain.on_event = lambda ev: None
    drain.call("register_channel", channel=77)
    s1 = Local(T1.sock)
    payload = None
    for size in (256 * 1024, 128 * 1024, 65536, 60000, 48000, 32768, 16384):
        p = b64(os.urandom(size))
        r = s1.call("send", to=L.key, channel=77, reliable=True, payload=p)
        if r.get("kind") != "error":
            payload, counters["msg_bytes"] = p, size
            break
    counters["msg_size_note"] = "largest accepted reliable message %d B (256 KiB requested)" % counters.get("msg_bytes", 0)
    pend = {"n": 0}
    lk = threading.Lock()

    def on_ev(ev):
        if ev.get("event") == "message_result":
            with lk:
                pend["n"] -= 1
                counters["msg_" + ev["status"]] = counters.get("msg_" + ev["status"], 0) + 1
    s1.on_event = on_ev

    def flood_msgs():
        while not stop_evt.is_set():
            with lk:
                ok = pend["n"] < 4
            if not ok:
                time.sleep(0.002)
                continue
            try:
                r = s1.call("send", to=L.key, channel=77, reliable=True, payload=payload, timeout=10)
                if r.get("kind") == "error":
                    counters["msg_errors"] = counters.get("msg_errors", 0) + 1
                    time.sleep(0.05)
                    continue
                with lk:
                    pend["n"] += 1
                counters["msgs_sent"] = counters.get("msgs_sent", 0) + 1
            except Exception:
                time.sleep(0.05)

    w = Local(L.sock)

    def flood_state():
        # The daemon caps a live value at LIVE_VALUE_CAP (200 B) and a durable one at
        # DURABLE_VALUE_CAP (4096 B). The first version wrote 512 B for both, so every
        # live write (7 in 8) came back TooLarge and the flood was mostly errors.
        i = 0
        live_val = b64(b"x" * 192)
        durable_val = b64(b"y" * 2048)
        while not stop_evt.is_set():
            try:
                durable = (i % 8 == 0)
                r = w.call("write", scope="member", name="flood%d" % (i % 32), live=not durable,
                           persistent=durable, value=durable_val if durable else live_val, timeout=5)
                if r.get("kind") == "ok":
                    counters["writes"] = counters.get("writes", 0) + 1
                else:
                    counters["write_errors"] = counters.get("write_errors", 0) + 1
                    code = "write_err_%s" % (r.get("code") or r.get("error") or r.get("message") or "?")
                    counters[code] = counters.get(code, 0) + 1
            except Exception:
                time.sleep(0.05)
            i += 1
            if i % 4 == 0:
                time.sleep(0.001)
    for f in (flood_msgs, flood_state):
        threading.Thread(target=f, daemon=True).start()
    stress = None
    try:
        sp = stress_cmd()
        stress = Proc("stress", ["nice", "-n", "15", sp, "--cpu", "8", "--timeout", "600s"],
                      os.path.join(cl.logdir, "stress.log"))
    except Exception as e:
        counters["stress_error"] = str(e)
    return stress


def rt_hist_rows(cl):
    rows = {}
    for t in cl.T:
        st = cl.timeline[-1][1].get(t.name) if cl.timeline else None
        st = t.rt_status() if t.daemon.alive() else st
        rows[t.name + ".out_hop"] = hist_row(st["rt"]["out_hop"])
    Ls = cl.L.rt_status()
    rows["L.in_queue"] = hist_row(Ls["rt"]["in_queue"])
    rows["L.pose_hop"] = hist_row(Ls["rt"]["pose_hop"])
    v = cl.L.vstats()
    rows["L.voice.ipc_delay_us"] = hist_row(v["histograms"]["ipc_delay_us"])
    rows["L.voice.listener_age_us"] = hist_row(v["histograms"]["listener_age_us"])
    return rows, v


def mk_cluster(run, args, audio, n=7, name="c", device=True, l_offset=0):
    cl = Cluster(run, args.prim, audio, name)
    cl.start_daemons(n, l_offset)
    if device:
        mic = PFX + "MIC"
        audio.add_mic(mic)
        sink = audio.add_sink(PFX + "sink-a")
        cl.sinks = [sink]
        cl.alsa_home(sink)
    return cl


def speech_specs(clips, tone_idx=()):
    specs = []
    for i in range(7):
        cap = "tone:auto" if i in tone_idx else "wav:" + clips[CLIP_ORDER[i]]
        specs.append((cap, SKEWS[i]))
    return specs


# ---------------------------------------------------------------- scenario: baseline / bulk / listener
def run_load(args, res, audio, scen, bulk, dur, tone_idx=(6,), idle_ok=False):
    run = os.path.join(res.run, scen)
    os.makedirs(run, exist_ok=True)
    cl = mk_cluster(run, args, audio, name=scen)
    counters = {}
    stop = threading.Event()
    stress = None
    try:
        if bulk:
            stress = bulk_helpers(cl, run, stop, counters)
        cl.start_listener("device", device=True)
        rec = Recorder(cl.sinks[0], cl.logdir)
        cl.rec_list.append(rec)
        cl.pollers()
        cl.start_talkers(speech_specs(args.clips, tone_idx))
        t_start = mono()
        ok = cl.wait_flowing()
        res.oracle(scen, "1a", "all %d streams reach L" % len(cl.T), ok, "waited %.1fs" % (mono() - t_start))
        t_flow = mono()
        time.sleep(2.0)
        u0 = (cl.L.vstats() or {}).get("underruns", 0)
        cpu0, w0 = proc_cpu_s(cl.L.voice.pid), mono()
        time.sleep(max(1, dur))
        cpu1, w1 = proc_cpu_s(cl.L.voice.pid), mono()
        s_end = cl.L.vstats()
        hist_rows, v = rt_hist_rows(cl)
        t_end = mono()
        counters_daemon = {t.name: t.rt_status()["rt"]["drops"] for t in cl.T}
        rec_x, i0, c0 = rec.slice_t(t_flow + 2.0, t_end)
    finally:
        stop.set()
        cl.stop()
        if stress:
            stress.kill_group()
    # --- evaluate
    peers = v["peers"]
    for t in cl.T:
        p = peers.get(t.key, {})
        r = p.get("receiver", {})
        total = max(1, p.get("blocks_pulled", 0) * 480)
        # The timeline receiver's own plc_samples (real Opus PLC, plus the FEC-recovered
        # samples counted apart as fec_samples) over the samples it produced while a talkspurt
        # was playing. Digital silence between talkspurts is idle_samples, not output, so
        # output_samples is the active-speech window; blocks_pulled also counts silent blocks
        # and would dilute the figure (kept as concealed_all_pct). On the old NetEq receiver the
        # same keys carry NetEq's concealed_samples / neteq_output_samples.
        cs = rget(r, "plc_samples", "concealed_samples", default=0)
        act = rget(r, "output_samples", "neteq_output_samples", default=0)
        secs = max(1e-9, p.get("blocks_pulled", 0) / 100.0)
        res.data.setdefault(scen, {})[t.name] = {"accepted": p.get("packets_accepted"), "blocks": p.get("blocks_pulled"),
                                                 "blocks_skipped": p.get("blocks_skipped"),
                                                 "concealed_active_pct": 100.0 * cs / max(1, act),
                                                 "neteq_output_samples": act,
                                                 "concealed_all_pct": 100.0 * cs / total,
                                                 "plc_samples": rget(r, "plc_samples", "concealed_samples"),
                                                 "fec_samples": r.get("fec_samples"),
                                                 "late_discarded": rget(r, "late", "late_packets_discarded"),
                                                 "duplicate": r.get("duplicate"),
                                                 "rejected": r.get("rejected"),
                                                 "local_late": r.get("local_late"),
                                                 "reordered_accepted": r.get("reordered_accepted"),
                                                 "accelerate_ops": r.get("accelerate_ops"),
                                                 "preemptive_ops": r.get("preemptive_ops"),
                                                 "playout_timestamp": r.get("playout_timestamp"),
                                                 "idle": r.get("idle"),
                                                 "decoded_per_sec": r.get("decoded_per_sec"),
                                                 "expand_per_sec": r.get("expand_per_sec"),
                                                 "accelerate_per_sec": r.get("accelerate_per_sec"),
                                                 "preemptive_expand_per_sec": r.get("preemptive_expand_per_sec"),
                                                 "concealment_events": r.get("concealment_events"),
                                                 "talkspurt_flushes": r.get("talkspurt_flushes"),
                                                 "talkspurt_flushes_per_min": 60.0 * (r.get("talkspurt_flushes") or 0) / secs}
        res.data[scen][t.name].update(peer_means(cl.L.statlog, t.key, t_flow + 10))
    worst = max((d["concealed_active_pct"] for d in res.data[scen].values()), default=0)
    dec = all((d["accepted"] or 0) > 0 and (d["blocks"] or 0) > 0 for d in res.data[scen].values())
    cpu_pct = None if None in (cpu0, cpu1) else 100.0 * (cpu1 - cpu0) / max(1e-9, w1 - w0)
    res.data[scen]["listener_cpu_pct_of_core"] = cpu_pct
    res.data[scen]["mixer"] = {"stages_us_per_block": mixer_rows(v), "counters": v.get("mixer")}
    res.data[scen]["opus_version"] = v.get("opus_version")
    res.oracle(scen, "1b", "every stream decoded at L; concealment < 2 %", dec and worst < 2.0,
               "worst concealment (Opus PLC) during active speech %.2f %% of receiver output; per-peer (accepted, active %%, flushes/min, late, dup, rejected, local_late) %s" % (worst, {k: (d["accepted"], round(d["concealed_active_pct"], 2), round(d["talkspurt_flushes_per_min"], 1), d["late_discarded"], d["duplicate"], d["rejected"], d["local_late"]) for k, d in res.data[scen].items() if isinstance(d, dict) and "accepted" in d}))
    res.oracle(scen, "1c", "no output underruns after the first second", v["underruns"] - u0 == 0,
               "underruns %d total, %d after first 2s (callback_gap p99 %s us)" % (v["underruns"], v["underruns"] - u0, {"n": v["histograms"]["callback_gap_us"]["count"], "p99_us_upper": bucket_pct(v["histograms"]["callback_gap_us"]["log2_buckets"], 99), "max_us": v["histograms"]["callback_gap_us"]["max"]}))
    mr = res.data[scen]["mixer"]["stages_us_per_block"]
    res.oracle(scen, "1c-mixer", "mixer block and per-stage timing (info; per block, all senders)", None,
               {"stages_us": mr, "peer_blocks": v.get("mixer"), "listener_cpu_pct_of_one_core": None if cpu_pct is None else round(cpu_pct, 2),
                "per_peer_buffer_ms_mean": {t.name: round(res.data[scen][t.name]["buffer_ms_mean"] or 0, 1) for t in cl.T},
                "per_peer_target_ms_mean": {t.name: round(res.data[scen][t.name]["target_delay_ms_mean"] or 0, 1) for t in cl.T},
                "listener_age_us": hist_row(v["histograms"]["listener_age_us"])})
    # device oracle: non-silent, level vs stats via T7 tone
    lvl = rms_db(rec_x)
    detail = "monitor rms %.1f dBFS over %.0fs" % (lvl, len(rec_x) / RATE)
    ok = lvl > -60
    if 6 in tone_idx:
        inc = peers[cl.T[6].key]["incarnation"]
        hz = auto_hz(inc)
        # Both sides over the same last N seconds, 1 s windows, power-averaged. The
        # recording is the whole mix and the stats' per-second Goertzel is too, so the
        # other talkers' speech near the tone frequency leaks into both; 50 ms windows
        # (20 Hz bins) made that +4.6 dB at 2160 Hz, and medians of two different
        # window sets still differed by 1.6 dB. Averaging power over 30 windows each
        # side shrinks the leakage noise well under the 1.5 dB limit.
        nwin = int(min(30, max(5, dur - 4)))
        xl, xr = rec.slice_st(t_end - nwin - 0.5, t_end - 0.5)
        pw = []
        for xx in (xl, xr):
            sq = [a * a for _, a in tone_series(xx, hz, win=RATE, hop=RATE // 2)]
            pw.append(sum(sq) / len(sq) if sq else 0.0)
        rec_amp = math.sqrt((pw[0] + pw[1]) / 2)
        sj = s_end.get("tone_seconds", [])
        sa = []
        for e in sj[-nwin - 1:-1]:
            for tn in e["tones"]:
                if tn["incarnation"] == inc:
                    sa.append(((tn["l"] ** 2 + tn["r"] ** 2) / 2))
        stat_amp = math.sqrt(sum(sa) / len(sa)) if sa else 0.0
        diff = 20 * math.log10(max(rec_amp, 1e-9) / max(stat_amp, 1e-9))
        ok = ok and abs(diff) < 1.5 and stat_amp > 0
        detail += "; T7 tone %.0f Hz: recorded %.4f vs stats %.4f (%.2f dB)" % (hz, rec_amp, stat_amp, diff)
    res.oracle(scen, "1d", "null-sink monitor non-silent and matches stats", ok, detail)
    # hops
    res.data[scen]["hops"] = hist_rows
    # drift
    drift = {}
    ok = True
    for t in cl.T:
        if abs(SKEWS[int(t.name[1:]) - 1]) < 500:
            continue
        pts = [(tt, s["peers"].get(t.key, {}).get("receiver", {})) for tt, s in cl.L.statlog]
        pts = [(tt - t_flow, r) for tt, r in pts if r]
        def m(lo, hi, k):
            vs = [r.get(k) for tt, r in pts if lo <= tt <= hi and r.get(k) is not None]
            return sum(vs) / len(vs) if vs else None
        span = pts[-1][0] if pts else 0
        w = min(30, span / 3)
        f_t, l_t = m(10, 10 + w, "target_delay_ms"), m(span - w, span, "target_delay_ms")
        f_b, l_b = m(10, 10 + w, "buffer_ms"), m(span - w, span, "buffer_ms")
        drift[t.name] = {"target_first": f_t, "target_last": l_t, "buffer_first": f_b, "buffer_last": l_b, "window_s": w}
        if None in (f_t, l_t, f_b, l_b):
            ok = ok and idle_ok  # a talker that is silent in the window has no buffer to measure
        elif l_t > f_t + 15 or (not idle_ok and l_b > f_b + 15):  # buffer comparisons need continuous talk
            ok = False
    res.oracle(scen, "1e", "receiver target/buffer delay of +-500 ppm talkers does not trend up", ok, drift)
    return cl, hist_rows, v, counters, counters_daemon


def hop_pass(rows):
    """Evaluate oracle 2 over hop rows: per-hop p99.9 < 2 ms, pooled IPC p99 < 10 ms."""
    out_rows = [r for k, r in rows.items() if k.endswith(".out_hop") and r.get("n")]
    res = {"out_hop_worst_p99_9_us": max((r["p99_9"] for r in out_rows), default=None),
           "in_queue_p99_9_us": rows["L.in_queue"].get("p99_9"),
           "ipc_delay_p99_9_us": rows["L.voice.ipc_delay_us"].get("p99_9"),
           "out_hop_worst_p99_us": max((r["p99"] for r in out_rows), default=None),
           "ipc_delay_p99_us": rows["L.voice.ipc_delay_us"].get("p99")}
    vals = [res["out_hop_worst_p99_9_us"], res["in_queue_p99_9_us"], res["ipc_delay_p99_9_us"]]
    ok_hops = all(v is not None and v < 2000 for v in vals)
    both = (res["out_hop_worst_p99_us"] or 0) + (res["ipc_delay_p99_us"] or 0)
    res["two_ipc_hops_p99_sum_us"] = both
    return ok_hops, both < 10000, res


def scen_baseline(args, res, audio):
    cl, rows, v, c, _ = run_load(args, res, audio, "baseline", False, 120 * args.scale)
    la = hist_row(v["histograms"]["listener_age_us"])
    res.oracle("baseline", "2-base", "per-hop delay (baseline numbers, for comparison)", None, hop_pass(rows)[2])
    res.data["baseline"]["listener_age"] = la
    return rows, la


def scen_bulk(args, res, audio, base=None):
    cl, rows, v, c, dd = run_load(args, res, audio, "bulk", True, 60 * args.scale)
    ok_h, ok_p, nums = hop_pass(rows)
    res.oracle("bulk", "2a", "each hop p99.9 < 2 ms under bulk", ok_h, nums)
    res.oracle("bulk", "2b", "p99 of both IPC hops < 10 ms under bulk", ok_p, {"sum_of_p99_us": nums["two_ipc_hops_p99_sum_us"]})
    res.data["bulk"]["flood_counters"] = c
    res.data["bulk"]["talker_rt_drops"] = dd
    if base:
        res.data["bulk"]["baseline_rows"] = base[0]
    la = hist_row(v["histograms"]["listener_age_us"])
    res.data["bulk"]["listener_age"] = la
    ok = (c.get("msg_delivered", 0) > 5 and c.get("writes", 0) > 100 and "stress_error" not in c
          and c.get("write_errors", 0) < 0.05 * max(1, c.get("writes", 0)))
    c["note"] = ("reliable messages are capped at 64 KiB (room_core MESSAGE_CAP); kept at 64 KiB, a 256 KiB request is "
                 "refused TooLarge. State writes: live values <= 200 B, durable <= 4096 B (staterec caps); "
                 "the flood writes 192 B live and 2048 B durable; write_errors must be < 5 % of writes.")
    res.oracle("bulk", "2c", "load was actually applied (state-write errors < 5 %)", ok, c)
    return rows, la


def scen_listener(args, res, audio, base_la=None, bulk_la=None):
    lim = 1e6 / 90 + 4000
    if base_la is None:
        base_la = scen_baseline_short(args, res, audio, False)
    if bulk_la is None:
        bulk_la = scen_baseline_short(args, res, audio, True)
    for tag, la in (("quiet", base_la), ("bulk", bulk_la)):
        res.oracle("listener", "3-" + tag, "listener-age p99 < %.1f ms (%s)" % (lim / 1000, tag),
                   la.get("p99") is not None and la["p99"] < lim, la)


def scen_baseline_short(args, res, audio, bulk):
    scen = "listener-" + ("bulk" if bulk else "quiet")
    cl, rows, v, c, _ = run_load(args, res, audio, scen, bulk, 40 * args.scale)
    return hist_row(v["histograms"]["listener_age_us"])


# ---------------------------------------------------------------- faults
class VoiceWatch:
    """Subscribe to member/*/voice at a daemon; timeline of (t, key, flags, incarnation)."""

    def __init__(self, node):
        self.c = Local(node.sock)
        self.rows = []
        self.c.on_event = self.ev
        self.c.call("subscribe", sub=1, pattern="member/*/voice")

    def ev(self, e):
        if e.get("event") != "updates":
            return
        for u in e["updates"]:
            if u.get("kind") == "value" and u["path"].endswith("/voice"):
                try:
                    raw = base64.b64decode(u["value"])
                    self.rows.append((mono(), u["path"].split("/")[1], raw[1], struct.unpack("<I", raw[2:6])[0]))
                except Exception:
                    pass
            if u.get("kind") == "removed" and u["path"].endswith("/voice"):
                self.rows.append((mono(), u["path"].split("/")[1], None, None))

    def since(self, t, key):
        return [r for r in self.rows if r[0] >= t and r[1] == key]

    def wait(self, t, key, pred, timeout=5):
        end = mono() + timeout
        while mono() < end:
            for r in self.since(t, key):
                if pred(r):
                    return r
            time.sleep(0.01)
        return None


def poll_until(fn, timeout, step=0.005):
    end = mono() + timeout
    while mono() < end:
        v = fn()
        if v:
            return mono(), v
        time.sleep(step)
    return None, None


def scen_faults(args, res, audio):
    scen = "faults"
    run = os.path.join(res.run, scen)
    os.makedirs(run, exist_ok=True)
    cl = mk_cluster(run, args, audio, name=scen)
    L = cl.L
    T3 = cl.T[2]
    specs = speech_specs(args.clips, tone_idx=(2,))
    try:
        cl.start_listener("device", device=True)
        recA = Recorder(cl.sinks[0], cl.logdir)
        cl.rec_list.append(recA)
        cl.pollers()
        cl.start_talkers(specs)
        cl.wait_flowing()
        time.sleep(8)
        w_L = VoiceWatch(L)
        w_T1 = VoiceWatch(cl.T[0])
        inc3 = lambda: (L.vstats() or {}).get("peers", {}).get(T3.key, {}).get("incarnation")
        hz3 = auto_hz(inc3())

        def amp_series(rec, hz, t0, t1, win=2400, hop=480):
            x, i0, c = rec.slice_t(t0, t1)
            return [(c + (i0 + i + win // 2) / RATE, a) for i, a in tone_series(x, hz, win=win, hop=hop)]

        def base_amp(rec, hz, t0, t1):
            s = sorted(a for _, a in amp_series(rec, hz, t0, t1))
            return s[len(s) // 2] if s else 0

        # ---- A. daemon pauses
        for who, node in (("L", L), ("T5", cl.T[4])):
            tag = "4a-" + who
            rt_before = node.rt_status()["rt"]
            vs_before = L.vstats()
            pre_buf = max((r.get("buffer_ms", 0) for r in [p.get("receiver", {}) for p in vs_before["peers"].values()]), default=0)
            amp0 = base_amp(recA, hz3, mono() - 5, mono() - 0.5)
            ctr0 = node.rt_status()["rt"]["received" if who == "L" else "sent"]
            ts, tc = mono(), None
            node.daemon.sig(signal.SIGSTOP)
            time.sleep(0.5)
            node.daemon.sig(signal.SIGCONT)
            tc = mono()
            def moved():
                try:
                    return node.local.status(1)["rt"]["received" if who == "L" else "sent"] > ctr0 + 2
                except Exception:
                    return False
            tflow, _ = poll_until(moved, 3)
            time.sleep(4)
            rt_after = node.rt_status()["rt"]
            vs_after = L.vstats()
            series = amp_series(recA, hz3, ts - 1, mono())
            # T3 tone recovery after SIGCONT (for L pause) / continuity (for T5 pause)
            if who == "L":
                rec_t = next((t for t, a in series if t >= tc and a >= 0.5 * amp0), None)
                resume_ms = (rec_t - tc) * 1000 if rec_t else None
                flow_ms = (tflow - tc) * 1000 if tflow else None
                rd = lambda k: rt_after["drops"].get(k, 0) - rt_before["drops"].get(k, 0)
                age_d = rd("age") + rd("age_media")
                vage_d = vs_after["drops"]["age"] - vs_before["drops"]["age"]
                maxbuf = max(max((p.get("receiver", {}).get("buffer_ms", 0) for p in s["peers"].values()), default=0)
                             for tt, s in L.statlog if tt >= tc)
                ipcmax = vs_after["histograms"]["ipc_delay_us"]["max"]
                res.oracle(scen, tag + "-resume", "L daemon paused 500 ms: audio resumes within 300 ms",
                           resume_ms is not None and resume_ms <= 300,
                           "tone back to 50%% in %s ms; VoiceIn flow resumed in %s ms" % (None if resume_ms is None else round(resume_ms), None if flow_ms is None else round(flow_ms)))
                res.oracle(scen, tag + "-stale", "stale inbound dropped by age (counter rises)", age_d + vage_d > 0,
                           "daemon drops.age +%d (age_media +%d, stale_incarnation +%d), voice drops.age +%d" % (age_d, rd("age_media"), rd("stale_incarnation"), vage_d))
                res.oracle(scen, tag + "-burst", "no burst of old audio (NetEq buffer within bound, nothing older than 100 ms)",
                           maxbuf <= pre_buf + 100 and ipcmax <= 100000,
                           "max NetEq buffer after resume %s ms (before: %s); max VoiceIn age at read %.1f ms" % (maxbuf, pre_buf, ipcmax / 1000))
            else:
                others = [t for t in cl.T if t.name not in ("T5",)]
                # The recording is the whole mix: the other six talkers are speech, and in a
                # 50 ms window (20 Hz bins) their energy near T3's frequency beats with the
                # tone (the same stream swings 0.7x..1.85x with no gap at all). 200 ms windows
                # cut that leakage ~4x. The dip is judged against the 5th percentile of the
                # same windows in the quiet seconds before, so the metric's own noise is
                # in the baseline; a real stall (underrun) is also checked directly.
                W, H = 9600, 1200
                wser = amp_series(recA, hz3, ts - 1, mono(), win=W, hop=H)
                base_w = sorted(a for t, a in amp_series(recA, hz3, ts - 6, ts - 0.7, win=W, hop=H))
                base_p5 = base_w[len(base_w) // 20] if base_w else 0.0
                base_med = base_w[len(base_w) // 2] if base_w else 0.0
                tt_ser = [(t, a) for t, a in wser if ts <= t <= tc + 0.3]
                tt_amp = [a for t, a in tt_ser]
                dip = min(tt_amp) / base_p5 if tt_amp and base_p5 else None
                amp0 = base_med or amp0
                # What was happening at the dip: recorder health (pw-record chunk gaps and
                # frames-vs-wall), L's output underruns, T3's concealment, T3's NetEq buffer.
                dip_t = min(tt_ser, key=lambda e: e[1])[0] if tt_ser else None
                with recA.lock:
                    chunks = [(t, fr) for t, fr in recA.times if ts - 0.5 <= t <= tc + 0.5]
                rec_gap = max((b[0] - a[0] for a, b in zip(chunks, chunks[1:])), default=0) * 1000
                rec_surplus = max((((b[1] - a[1]) / RATE) - (b[0] - a[0]) for a, b in zip(chunks, chunks[1:])), default=0) * 1000
                p3b = vs_before["peers"].get(T3.key, {})
                p3a = vs_after["peers"].get(T3.key, {})
                def d3(k, scope="receiver"):
                    return (p3a.get(scope, {}).get(k, 0) or 0) - (p3b.get(scope, {}).get(k, 0) or 0)
                t3buf = [s["peers"].get(T3.key, {}).get("receiver", {}).get("buffer_ms") for tt, s in L.statlog if ts - 1 <= tt <= tc + 2]
                dip_diag = {"dip_at_ms_after_cont": None if dip_t is None else round((dip_t - tc) * 1000),
                            "rec_chunk_gap_max_ms": round(rec_gap, 1), "rec_frames_over_wall_max_ms": round(rec_surplus, 1),
                            "L_underruns_delta": vs_after["underruns"] - vs_before["underruns"],
                            "baseline_p5_over_median": round(base_p5 / base_med, 2) if base_med else None,
                            "T3_concealed_samples_delta": d3("concealed_samples"),
                            "T3_expand_events_delta": d3("concealment_events"),
                            "T3_packets_accepted_delta": d3("packets_accepted", scope=None) if False else (p3a.get("packets_accepted", 0) - p3b.get("packets_accepted", 0)),
                            "T3_buffer_ms_1hz": t3buf,
                            "series_ms_after_cont_amp_over_normal": [(round((t - tc) * 1000), round(a / amp0, 2)) for t, a in tt_ser if amp0]}
                res.data.setdefault(scen, {})["T5_others_dip_diag"] = dip_diag
                flow_ms = (tflow - tc) * 1000 if tflow else None
                rd = lambda k: rt_after["drops"].get(k, 0) - rt_before["drops"].get(k, 0)
                age_d = rd("age") + rd("out_age")
                v5 = cl.T[4].vstats()
                drop5 = sum((v5 or {}).get("drops", {}).get(k, 0) for k in ("age", "send_socket_full"))
                res.oracle(scen, tag + "-resume", "T5 daemon paused 500 ms: T5 flow resumes within 300 ms",
                           flow_ms is not None and flow_ms <= 300, "datagram flow resumed in %s ms" % (None if flow_ms is None else round(flow_ms)))
                res.oracle(scen, tag + "-others", "other talkers unaffected by a talker-daemon pause (T3 tone min %.0f%% of baseline p5, no underruns)" % (100 * (dip or 0)),
                           dip is not None and dip > 0.7 and dip_diag["L_underruns_delta"] == 0, "T3 tone (200 ms windows) min / baseline-p5 = %s; %s" % (None if dip is None else round(dip, 3), {k: v for k, v in dip_diag.items() if k != "series_ms_after_cont_amp_over_normal"}))
                res.oracle(scen, tag + "-stale", "T5's stale frames dropped by age/size (counter rises)", age_d + drop5 > 0,
                           "T5 daemon drops.age+out_age +%d (out_age +%d), T5 voice age/send_full drops %d" % (age_d, rd("out_age"), drop5))
                maxbuf = max(max((p.get("receiver", {}).get("buffer_ms", 0) for p in [s["peers"].get(cl.T[4].key, {})]), default=0) for tt, s in L.statlog if tt >= tc)
                res.oracle(scen, tag + "-burst", "no burst of old T5 audio at L", maxbuf <= pre_buf + 100, "max T5 NetEq buffer after resume %s ms (before max %s)" % (maxbuf, pre_buf))
            time.sleep(3)

        # ---- A2. voice-client pause (4e): the daemon keeps sending VoiceIn into the
        # client's socket for 1.5 s (under the 2 s lease TTL); on resume the backlog must
        # be dropped by age, not replayed.
        rt_b = L.rt_status()["rt"]
        vs_b = L.vstats()
        pre_buf = max((r.get("buffer_ms", 0) for r in [p.get("receiver", {}) for p in vs_b["peers"].values()]), default=0)
        amp0 = base_amp(recA, hz3, mono() - 5, mono() - 0.5)
        ts = mono()
        L.voice.sig(signal.SIGSTOP)
        time.sleep(1.5)
        L.voice.sig(signal.SIGCONT)
        tc = mono()
        time.sleep(5)
        rt_a = L.rt_status()["rt"]
        vs_a = L.vstats()
        rdk = lambda k: rt_a["drops"].get(k, 0) - rt_b["drops"].get(k, 0)
        vdk = lambda k: vs_a["drops"].get(k, 0) - vs_b["drops"].get(k, 0)
        maxbuf = max(max((p.get("receiver", {}).get("buffer_ms", 0) for p in s["peers"].values()), default=0)
                     for tt, s in L.statlog if tt >= tc)
        counted = vdk("age") + rdk("age") + rdk("age_media") + rdk("queue_full") + rdk("per_sender")
        mm = vs_a.get("clock_mismatch", 0) - vs_b.get("clock_mismatch", 0)
        ser = amp_series(recA, hz3, tc, mono())
        rec_t = next((t for t, a in ser if a >= 0.5 * amp0), None) if amp0 else None
        res.oracle(scen, "4e", "L voice client SIGSTOP 1.5 s: no old audio replayed (NetEq buffer within the 4a-L-burst bound), drops counted, audio resumes",
                   maxbuf <= pre_buf + 100 and counted > 0 and mm == 0 and rec_t is not None,
                   "max NetEq buffer after resume %s ms (before: %s); counted drops: voice age +%d, daemon age +%d / age_media +%d / queue_full +%d / per_sender +%d / stale_incarnation +%d; clock_mismatch +%d; tone back to 50%% in %s ms"
                   % (maxbuf, pre_buf, vdk("age"), rdk("age"), rdk("age_media"), rdk("queue_full"), rdk("per_sender"), rdk("stale_incarnation"), mm,
                      None if rec_t is None else round((rec_t - tc) * 1000)))
        time.sleep(2)

        # ---- B. voice crash and restart: T3
        hz_old = hz3
        tk = mono()
        sent0 = T3.rt_status()["rt"]["sent"]
        T3.voice.kill9()
        r = w_L.wait(tk, T3.key, lambda r: r[2] == 0 or r[2] is None, 3)
        res.oracle(scen, "4b-1", "T3 voice killed: key flags go 0 at L", r is not None, "after %s ms" % (None if not r else round((r[0] - tk) * 1000)))
        fence0 = L.vstats()["fence"]["new_stream"]
        inc_before = inc3()
        time.sleep(1.0)
        ts = mono()
        T3.start_voice("tone:auto", "null", tag="b")
        tfirst, _ = poll_until(lambda: T3.rt_status()["rt"]["sent"] > sent0, 5, 0.003)
        r = w_L.wait(ts, T3.key, lambda r: r[2] not in (0, None), 4)
        inc_new = r[3] if r else None
        res.oracle(scen, "4b-2", "T3 restart: key flags nonzero again with a new incarnation at L", r is not None and inc_new != inc_before, "inc %s -> %s" % (inc_before, inc_new))
        time.sleep(3)
        hz_new = auto_hz(inc3())
        x, i0, c = recA.slice_t(ts - 0.5, mono())
        sn = tone_series(x, hz_new)
        floor = sorted(a for _, a in tone_series(x, 2320.0))
        floor = floor[len(floor) // 2] if floor else 0
        on = next((c + (i0 + i + 1200) / RATE for i, a in sn if a > max(0.02, 10 * floor)), None)
        acc_ms = (on - tfirst) * 1000 if on and tfirst else None
        v = L.vstats()
        res.oracle(scen, "4b-3", "new incarnation accepted at L; audible within 200 ms of first packet (+100 ms playout allowance, 1 Hz stats)",
                   acc_ms is not None and acc_ms <= 300 and v["fence"]["new_stream"] > fence0,
                   "tone onset %.0f ms after first packet; fence.new_stream %d -> %d; stats granularity 1 s" % (acc_ms if acc_ms else -1, fence0, v["fence"]["new_stream"]))
        # L voice crash
        tk = mono()
        holders_before = [h["role"] for h in L.rt_status()["rt"]["holders"]]
        L.voice.kill9()
        t_rel, _ = poll_until(lambda: "voice" not in [h["role"] for h in L.rt_status(1)["rt"]["holders"]], 3, 0.005)
        r = w_T1.wait(tk, L.key, lambda r: r[2] == 0 or r[2] is None, 3)
        res.oracle(scen, "4b-4", "L voice killed: lease released at once, L's key flags 0 at a talker", t_rel is not None and t_rel - tk < 0.3 and r is not None,
                   "lease gone %s ms, key flags 0 at T1 after %s ms (holders before %s)" % (None if not t_rel else round((t_rel - tk) * 1000), None if not r else round((r[0] - tk) * 1000), holders_before))
        time.sleep(1.0)
        ts = mono()
        cl.start_listener("device", tracker=False, device=True, quiet=cl.T)
        r = w_T1.wait(ts, L.key, lambda r: r[2] not in (0, None), 4)
        time.sleep(4)
        x, i0, c = recA.slice_t(ts + 2, mono())
        amp = sorted(a for _, a in tone_series(x, auto_hz(inc3())))
        amp = amp[len(amp) // 2] if amp else 0
        res.oracle(scen, "4b-5", "L voice restart reacquires the lease; audio resumes", r is not None and amp > 0.02,
                   "key nonzero at T1 after %s ms; T3 tone median %.3f" % (None if not r else round((r[0] - ts) * 1000), amp))
        p = subprocess.run([cl.prim, "voice", "--rt-socket", L.rt, "--capture", "none", "--output", "null"], capture_output=True, text=True, timeout=15)
        res.oracle(scen, "4b-6", "second listener without --takeover fails visibly (Refused busy)", p.returncode == 3 and "refused" in (p.stderr + p.stdout).lower(),
                   "exit %d, %r" % (p.returncode, p.stderr.strip()[:100]))

        # ---- C. device change
        sinkA = cl.sinks[0]
        sinkB = audio.add_sink(PFX + "sink-b")
        recB = Recorder(sinkB, cl.logdir)
        cl.rec_list.append(recB)
        time.sleep(1)
        def gen_info():
            s = L.vstats() or {}
            return s.get("device_generation"), s.get("incarnation")
        def wait_gen(g, timeout=4):
            return poll_until(lambda: (L.vstats() or {}).get("device_generation", 0) > g, timeout, 0.05)[0]
        def tone_at(rec, t0, t1):
            x, i0, c = rec.slice_t(t0, t1)
            s = sorted(a for _, a in tone_series(x, auto_hz(inc3())))
            return s[len(s) // 2] if s else 0
        g0, i0_ = gen_info()
        ts = mono()
        L.voice.sig(signal.SIGUSR1)
        tg = wait_gen(g0)
        time.sleep(3)
        g1, i1 = gen_info()
        lk = audio.stream_targets()
        res.oracle(scen, "4c-1", "SIGUSR1: device generation bumps, incarnation unchanged, audio resumes",
                   g1 == g0 + 1 and i1 == i0_ and tone_at(recA, mono() - 2, mono()) > 0.02,
                   "gen %s -> %s, incarnation %s -> %s, tone on sink A %.3f, links %s" % (g0, g1, i0_, i1, tone_at(recA, mono() - 2, mono()), {k: sorted(v) for k, v in lk.items()}))
        # move stream A -> B
        pid = L.voice.pid
        sid = None
        out = sh(["pactl", "list", "sink-inputs"])
        for blk in out.split("Sink Input #")[1:]:
            if "ALSA [prim]" in blk and "Sink: " in blk:
                sid = blk.split("\n", 1)[0].strip()
        ok_move = False
        if sid:
            sh(["pactl", "move-sink-input", sid, sinkB], check=False)
            time.sleep(2)
            ta, tb = tone_at(recA, mono() - 1, mono()), tone_at(recB, mono() - 1, mono())
            ok_move = tb > 0.02 and ta < 0.005
            res.oracle(scen, "4c-2", "pactl move-sink-input A -> B: audio continues on B only", ok_move and not audio.check_links(),
                       "sink-input %s, tone on A %.4f, on B %.4f, gen %s (app is not told of a graph move)" % (sid, ta, tb, gen_info()[0]))
        else:
            res.oracle(scen, "4c-2", "pactl move-sink-input A -> B", False, "no sink-input found for pid %d" % pid)
        # unload the sink L is on (B or A), reopen must land on the other
        cur, other = (sinkB, sinkA) if ok_move else (sinkA, sinkB)
        cl.set_sink(other)  # the ALSA default now points at the other null sink
        g0, i0_ = gen_info()
        tu = mono()
        audio.rm_sink(cur)
        stall0 = (L.vstats() or {}).get("device_stall_reopen", 0)
        auto = wait_gen(g0, 3)
        how = "automatic reopen %.0f ms after the unload (watchdog)" % ((auto - tu) * 1000) if auto else ""
        if not auto:
            how = "no automatic reopen after sink removal; SIGUSR1 sent"
            L.voice.sig(signal.SIGUSR1)
            wait_gen(g0, 4)
        stall1 = (L.vstats() or {}).get("device_stall_reopen", 0)
        time.sleep(3)
        g1, i1 = gen_info()
        rec_other = recA if other == sinkA else recB
        tn = tone_at(rec_other, mono() - 2, mono())
        links = {k: sorted(v) for k, v in audio.stream_targets().items()}
        res.oracle(scen, "4c-3", "null sink L is on unloaded: reopen lands on the other null sink, gen bumps, incarnation unchanged, audio resumes",
                   bool(auto) and stall1 > stall0 and g1 > g0 and i1 == i0_ and tn > 0.02 and not audio.check_links() and links.get("alsa_playback.prim") == [other],
                   "%s; device_stall_reopen %s -> %s; gen %s -> %s; tone on %s %.3f; links %s" % (how, stall0, stall1, g0, g1, other, tn, links))
        res.data.setdefault(scen, {})["device_reopen_note"] = how
    finally:
        cl.stop()
    # ---- D. mismatched clocks
    for off in (5_000_000, -5_000_000):
        c2 = mk_cluster(run, args, audio, n=2, name="clk%d" % (off // 1000000), device=False, l_offset=off)
        try:
            c2.start_listener("null", tracker=False, device=False)
            c2.pollers()
            c2.start_talkers(speech_specs(args.clips)[:2])
            c2.wait_flowing()
            time.sleep(10 * max(args.scale, 0.3))
            v = c2.L.vstats()
            peers = v["peers"]
            dec = all(p.get("packets_accepted", 0) > 0 and p.get("blocks_pulled", 0) > 0 for p in peers.values()) and len(peers) == 2
            conc = max(p["receiver"]["concealed_samples"] / max(1, p["receiver"].get("neteq_output_samples", 0)) * 100 for p in peers.values() if p.get("receiver"))
            res.oracle(scen, "4d%+d" % (off // 1000000), "daemon clock offset %+d s: clock_mismatch counts rise, audio still decodes" % (off // 1000000),
                       v["clock_mismatch"] > 0 and dec, "clock_mismatch %d, accepted %s, worst active-speech concealment %.1f %%" % (v["clock_mismatch"], [p["packets_accepted"] for p in peers.values()], conc))
        finally:
            c2.stop()


# ---------------------------------------------------------------- stale buffers
def scen_stale(args, res, audio):
    scen = "stale"
    run = os.path.join(res.run, scen)
    os.makedirs(run, exist_ok=True)
    cl = mk_cluster(run, args, audio, name=scen)
    L, T7 = cl.L, cl.T[6]
    try:
        cl.start_listener("device", device=True)
        rec = Recorder(cl.sinks[0], cl.logdir)
        cl.rec_list.append(rec)
        cl.pollers()
        cl.start_talkers(speech_specs(args.clips, tone_idx=(6,)))
        cl.wait_flowing()
        time.sleep(6)
        for rnd, delay in enumerate((0.05, 1.0), 1):
            inc_old = L.vstats()["peers"][T7.key]["incarnation"]
            hz_old = auto_hz(inc_old)
            c0 = {"wrong_inc": T7.rt_status()["rt"]["drops"]["wrong_incarnation"], "Lstale": L.vstats()["fence"]["stale"],
                  "Lnew": L.vstats()["fence"]["new_stream"], "Ldrops": dict(L.rt_status()["rt"]["drops"])}
            tk = mono()
            T7.voice.kill9()
            time.sleep(delay)
            ts = mono()
            T7.start_voice("tone:auto", "null", tag="r%d" % rnd)
            for _ in range(100):
                if L.vstats()["peers"][T7.key]["incarnation"] != inc_old:
                    break
                time.sleep(0.1)
            time.sleep(4)
            inc_new = L.vstats()["peers"][T7.key]["incarnation"]
            hz_new = auto_hz(inc_new)
            x, i0, c = rec.slice_t(tk - 1, mono())
            t_of = lambda i: c + (i0 + i + 1200) / RATE
            sn = tone_series(x, hz_new)
            on = next((t_of(i) for i, a in sn if t_of(i) >= tk and a > 0.1), None)
            fl = sorted(a for _, a in tone_series(x, 2320.0))
            floor = fl[len(fl) // 2] if fl else 0
            so = [(t_of(i), a) for i, a in tone_series(x, hz_old)]
            win = [a for t, a in so if on and on <= t <= on + 1.0]
            peak_before = max((a for t, a in so if t < tk), default=0)
            worst = max(win) if win else None
            late = [a for t, a in so if on and on + 0.8 <= t <= on + 1.0]
            lmax = max(late) if late else None
            # reference: what unused neighbouring 20 Hz-grid frequencies show in the same window (other talkers' speech)
            nb = []
            for dh in (-100, -80, -60, -40, 40, 60, 80, 100):
                nb += [a for i, a in tone_series(x, hz_old + dh) if on and on + 0.8 <= t_of(i) <= on + 1.0]
            floor = max(nb) if nb else floor
            ok = on is not None and lmax is not None and lmax <= max(2 * floor, 0.003) and peak_before > 0.02
            c1 = {"wrong_inc": T7.rt_status()["rt"]["drops"]["wrong_incarnation"], "Lstale": L.vstats()["fence"]["stale"],
                  "Lnew": L.vstats()["fence"]["new_stream"], "Ldrops": dict(L.rt_status()["rt"]["drops"])}
            res.oracle(scen, "5-%d" % rnd, "restart %d (delay %.2fs): old tone at noise floor by the end of the first output second of the new stream, new tone appears" % (rnd, delay), ok,
                       "old %.0f Hz peak before %.3f; max in [onset,+1s] %s, in last 0.2 s of that second %s (neighbour-frequency ceiling %.5f); new %.0f Hz onset %s s after kill" % (hz_old, peak_before, None if worst is None else round(worst, 5), None if lmax is None else round(lmax, 5), floor, hz_new, None if not on else round(on - tk, 2)))
            res.oracle(scen, "5-%d-counters" % rnd, "session counters after restart %d" % rnd, c1["Lnew"] > c0["Lnew"],
                       "L fence new_stream %d->%d, stale %d->%d; L daemon drops %s -> %s" % (c0["Lnew"], c1["Lnew"], c0["Lstale"], c1["Lstale"], c0["Ldrops"], c1["Ldrops"]))
    finally:
        cl.stop()


# ---------------------------------------------------------------- scenes
def read_f32(path):
    a = array.array("f")
    with open(path, "rb") as f:
        a.frombytes(f.read())
    return a


def read_wav_f32(path):
    with open(path, "rb") as f:
        d = f.read()
    i = d.find(b"data")
    n = struct.unpack("<I", d[i + 4:i + 8])[0]
    a = array.array("f")
    a.frombytes(d[i + 8:i + 8 + (n // 4) * 4] if n < len(d) else d[i + 8:len(d) - ((len(d) - i - 8) % 4)])
    return a


def read_mono_wav(path):
    w = wave.open(path)
    assert w.getframerate() == RATE and w.getnchannels() == 1
    raw = w.readframes(w.getnframes())
    return array.array("h", raw)


def write_wav16(path, chans, n=None):
    w = wave.open(path, "wb")
    w.setnchannels(len(chans))
    w.setsampwidth(2)
    w.setframerate(RATE)
    n = min(len(c) for c in chans)
    out = array.array("h", [0] * (n * len(chans)))
    for k, c in enumerate(chans):
        out[k::len(chans)] = array.array("h", [max(-32768, min(32767, int(round(v * 32767)))) for v in c[:n]])
    w.writeframes(out.tobytes())
    w.close()


def resample_44_48(x):
    """44.1 -> 48 kHz: out[m] interpolates the input at m*147/160 (Hann-windowed sinc, 48 taps)."""
    up, down, half = 160, 147, 24
    fc = 0.5 * 0.97
    tabs = []
    for p in range(up):
        fr = p / up
        row = []
        for k in range(-half + 1, half + 1):
            t = k - fr
            h = 2 * fc * (math.sin(2 * math.pi * fc * t) / (2 * math.pi * fc * t) if t else 1.0)
            row.append(h * (0.5 + 0.5 * math.cos(math.pi * t / half)))
        tabs.append(row)
    xp = [0.0] * half + list(x) + [0.0] * (half + 2)
    n_out = int((len(x) - 1) * up / down)
    out = array.array("f", [0.0] * n_out)
    for m in range(n_out):
        base, p = divmod(m * down, up)
        out[m] = math.sumprod(xp[base + 1:base + 1 + 2 * half], tabs[p])
    return out


def lowpass_fir(x, fc, taps=129):
    h = []
    mid = taps // 2
    for n in range(taps):
        k = n - mid
        s = 2 * fc / RATE if k == 0 else math.sin(2 * math.pi * fc * k / RATE) / (math.pi * k)
        w = 0.54 - 0.46 * math.cos(2 * math.pi * n / (taps - 1))
        h.append(s * w)
    g = sum(h)
    h = [v / g for v in h]
    xs = list(x)
    pad = [0.0] * mid
    xp = pad + xs + pad
    return [math.sumprod(xp[i:i + taps], h) for i in range(len(xs))]


def xcorr_peak(a, b, max_lag, step=1):
    """Lag (in samples) of b relative to a maximizing sum a[i]*b[i+lag]; coarse on decimated, refined."""
    return None


def align_lag(ref, x, max_lag):
    """Lag such that x[i+lag] ~ ref[i]: envelope correlation (1 ms), then sample-accurate refinement."""
    d = 48
    env = lambda s: [math.sqrt(math.sumprod(s[i:i + d], s[i:i + d])) for i in range(0, len(s) - d, d)]
    er, ex = env(ref), env(x)
    mr = sum(er) / len(er)
    er = [v - mr for v in er]
    best, bl = -1e30, 0
    for lag in range(-max_lag // d, max_lag // d + 1):
        lo, hi = max(0, -lag), min(len(er), len(ex) - lag)
        if hi - lo < len(er) // 2:
            continue
        c = math.sumprod(er[lo:hi], ex[lo + lag:hi + lag])
        if c > best:
            best, bl = c, lag
    c0 = bl * d
    n = min(len(ref), 96000)
    start = max(range(0, len(ref) - n + 1, 4800), key=lambda s: math.sumprod(ref[s:s + n:8], ref[s:s + n:8]))
    r = ref[start:start + n]
    best, bl2 = -1e30, c0
    for lag in range(c0 - 2 * d, c0 + 2 * d + 1):
        if start + lag < 0 or start + lag + n > len(x):
            continue
        c = math.sumprod(r, x[start + lag:start + lag + n])
        if c > best:
            best, bl2 = c, lag
    return bl2


def fft(a):
    n = len(a)
    j = 0
    for i in range(1, n):
        bit = n >> 1
        while j & bit:
            j ^= bit
            bit >>= 1
        j ^= bit
        if i < j:
            a[i], a[j] = a[j], a[i]
    ln = 2
    while ln <= n:
        ang = -2 * math.pi / ln
        wl = complex(math.cos(ang), math.sin(ang))
        for i in range(0, n, ln):
            w = 1 + 0j
            for k in range(i, i + ln // 2):
                u, v = a[k], a[k + ln // 2] * w
                a[k], a[k + ln // 2] = u + v, u - v
                w *= wl
        ln <<= 1
    return a


def band_levels(x, bands, n=2048):
    """Mean power per band (Hann, Welch) -> list of dB."""
    win = [0.5 - 0.5 * math.cos(2 * math.pi * i / n) for i in range(n)]
    acc = [0.0] * (n // 2)
    cnt = 0
    step = max(n, (len(x) // n // 40) * n)
    for s in range(0, len(x) - n, step):
        seg = [x[s + i] * win[i] for i in range(n)]
        if math.sumprod(seg, seg) < 1e-9:
            continue
        sp = fft([complex(v) for v in seg])
        for k in range(n // 2):
            acc[k] += sp[k].real ** 2 + sp[k].imag ** 2
        cnt += 1
    out = []
    for lo, hi in bands:
        ks = [k for k in range(n // 2) if lo <= k * RATE / n < hi]
        p = sum(acc[k] for k in ks) / max(1, len(ks) * max(1, cnt))
        out.append(10 * math.log10(max(p, 1e-30)))
    return out


def itd_us(l, r):
    """Positive: left ear leads (source toward the left)... returns lag of r relative to l in us (<0: r leads)."""
    ll, rr = lowpass_fir(l, 1500), lowpass_fir(r, 1500)
    m = 48
    best, bk = -1e30, 0
    for k in range(-m, m + 1):
        if k >= 0:
            c = math.sumprod(ll[:len(ll) - k], rr[k:])
        else:
            c = math.sumprod(ll[-k:], rr[:len(rr) + k])
        if c > best:
            best, bk = c, k
    return bk / RATE * 1e6  # + : right ear lags left (left leads)


SCENES = ["speech_0", "speech_45", "speech_-45", "speech_90", "speech_-90", "speech_180", "table-front", "table-around"]


def scen_scenes(args, res, audio):
    scen = "scenes"
    out = os.path.join(res.run, "scenes")
    os.makedirs(out, exist_ok=True)
    plan = {r["name"]: r for r in json.load(open(os.path.join(args.godot, "godot", "plan.json")))["renders"]}
    table = []
    for name in SCENES:
        voices = plan[name]["voices"]
        run = os.path.join(res.run, "scenes-run", name)
        os.makedirs(run, exist_ok=True)
        # pad every clip to a common loop period so a looped capture plays each exactly once per period
        # plan.json holds the absolute path from the machine that rendered it
        clips = [read_mono_wav(v["wav"] if os.path.exists(v["wav"]) else os.path.join(args.godot, "inputs", os.path.basename(v["wav"]))) for v in voices]
        period = max(len(c) for c in clips) + 2 * RATE
        padded = []
        for i, c in enumerate(clips):
            p = os.path.join(run, "in%d.wav" % i)
            w = wave.open(p, "wb"); w.setnchannels(1); w.setsampwidth(2); w.setframerate(RATE)
            w.writeframes((c.tobytes()) + bytes(2 * (period - len(c)))); w.close()
            padded.append(p)
        cl = Cluster(run, args.prim, audio, name="sc" + str(SCENES.index(name)))
        cl.start_daemons(len(voices))
        wavp = os.path.join(out, name + ".r0raw.wav")
        try:
            L = cl.L
            ex = []
            for t, v in zip(cl.T, voices):
                ex += ["--seat", "%s=%s" % (t.key[:8], v["azimuth"])]
            L.start_voice("none", "wav:" + wavp, extra=ex)
            time.sleep(1.0)
            for t, p in zip(cl.T, padded):
                t.start_voice("wav:" + p, "null")
            cl.wait_flowing()
            time.sleep(period / RATE + 1.5)
        finally:
            cl.stop()
        raw = read_wav_f32(wavp)
        r0 = [array.array("f", raw[0::2]), array.array("f", raw[1::2])]
        g = read_f32(os.path.join(args.godot, "godot", name + ".f32"))
        gl, gr = resample_44_48(array.array("f", g[0::2])), resample_44_48(array.array("f", g[1::2]))
        # align on the mono sum
        gm = [(a + b) * 0.5 for a, b in zip(gl, gr)]
        rm = [(a + b) * 0.5 for a, b in zip(r0[0], r0[1])]
        lag = align_lag(gm, rm, 3 * RATE)
        n = len(gm)
        a_l = list(r0[0][max(lag, 0):max(lag, 0) + n]) ; a_r = list(r0[1][max(lag, 0):max(lag, 0) + n])
        n = min(n, len(a_l))
        gl, gr = list(gl)[:n], list(gr)[:n]
        a_l, a_r = a_l[:n], a_r[:n]
        write_wav16(os.path.join(out, name + ".r0.wav"), [a_l, a_r])
        write_wav16(os.path.join(out, name + ".godot.wav"), [gl, gr])
        os.unlink(wavp)
        # analysis on the voiced middle
        trim = RATE // 4
        s = slice(trim, max(trim + 1, n - trim))
        row = {"scene": name, "lag_ms": lag / RATE * 1000}
        el = lambda x: 10 * math.log10(max(math.sumprod(x[s], x[s]) / max(1, len(x[s])), 1e-20))
        row["r0_L_dB"], row["r0_R_dB"], row["godot_L_dB"], row["godot_R_dB"] = el(a_l), el(a_r), el(gl), el(gr)
        tot = lambda l, r: 10 * math.log10(max((math.sumprod(l[s], l[s]) + math.sumprod(r[s], r[s])) / (2 * max(1, len(l[s]))), 1e-20))
        row["level_diff_dB"] = tot(a_l, a_r) - tot(gl, gr)
        row["r0_ILD_dB"] = row["r0_L_dB"] - row["r0_R_dB"]
        row["godot_ILD_dB"] = row["godot_L_dB"] - row["godot_R_dB"]
        if len(voices) == 1:
            row["ear_diff_dB"] = (row["r0_L_dB"] - row["godot_L_dB"], row["r0_R_dB"] - row["godot_R_dB"])
            row["r0_ITD_us"] = itd_us(a_l[s], a_r[s])
            row["godot_ITD_us"] = itd_us(gl[s], gr[s])
            bands = [(400, 1600), (1600, 6400), (6400, 16000)]
            b_r0 = (band_levels(a_l[s], bands), band_levels(a_r[s], bands))
            b_g = (band_levels(gl[s], bands), band_levels(gr[s], bands))
            row["ILD_bands_dB"] = {"%d-%d" % b: {"r0": round(b_r0[0][k] - b_r0[1][k], 2), "godot": round(b_g[0][k] - b_g[1][k], 2)} for k, b in enumerate(bands)}
        table.append(row)
        log("scene", name, {k: (round(v, 2) if isinstance(v, float) else v) for k, v in row.items() if k != "ILD_bands_dB"})
    res.data["scenes"] = table
    for r in table:
        ok = abs(r["level_diff_dB"]) < 3.0
        if "r0_ITD_us" in r and abs(r["godot_ITD_us"]) > 100:
            ok = ok and (r["r0_ITD_us"] * r["godot_ITD_us"] > 0) and (r["r0_ILD_dB"] * r["godot_ILD_dB"] > 0)
        res.oracle("scenes", "6-" + r["scene"], "aligned with Godot render: level within 3 dB, ILD/ITD same side", ok,
                   {k: (round(v, 2) if isinstance(v, float) else v) for k, v in r.items() if k != "scene"})


# ---------------------------------------------------------------- receiver oracles
# One steady tone through the whole path (talker -> daemons -> listener -> HRTF -> null sink),
# measured on the sink's recording. 440 Hz has a period of 109.09 samples, so a packet played
# at the wrong place in time is out of phase (600 Hz would divide 960 and hide it).
TONE_HZ = 440.0
TONE_DUR = 30.0
CLEAN_REF = {}  # the lossless tone run, measured once per harness run


def tone_run(args, res, audio, scen, label, specs, dur, rec_tail=4.0):
    """One listener and len(specs) tone talkers `(capture, skew_ppm, extra_flags)`. The talkers
    run `--duration dur` and exit cleanly (END marker), then the receiver plays out and idles."""
    run = os.path.join(res.run, scen)
    os.makedirs(run, exist_ok=True)
    cl = mk_cluster(run, args, audio, n=len(specs), name=label)
    out = {"label": label}
    try:
        cl.start_listener("device", device=True, tracker=False)
        # a long capture period: this recording is read whole, not aligned in time, and a stall of
        # the recorder itself must not look like a gap in the listener's output
        rec = Recorder(cl.sinks[0], cl.logdir, latency="100ms")
        cl.rec_list.append(rec)
        cl.pollers()
        t_start = mono()
        for t, (cap, skew, extra) in zip(cl.T, specs):
            t.start_voice(cap, "null", skew=skew, extra=list(extra) + ["--duration", str(dur)])
        out["flowing"] = cl.wait_flowing()
        t_flow = mono()
        time.sleep(2.0)
        out["u0"] = (cl.L.vstats() or {}).get("underruns", 0)
        for t in cl.T:
            try:
                t.voice.p.wait(dur + 40)
            except subprocess.TimeoutExpired:
                pass
        time.sleep(rec_tail)  # relay, play out, go idle
        out["v"] = cl.L.vstats()
        out["t"] = [t.vstats() for t in cl.T]
        out["keys"] = [t.key for t in cl.T]
        out["xl"], out["xr"] = rec.slice_st(t_start, mono())
        bad = [i for i, v in enumerate(out["xl"]) if not math.isfinite(v)]
        out["nonfinite"] = {"n": len(bad), "after_start": len([i for i in bad if i >= 4800]), "first": bad[:5], "runs": len([i for k, i in enumerate(bad) if k == 0 or bad[k - 1] != i - 1])}
        if bad:
            log("NON-FINITE samples in the recording of %s: %s" % (label, out["nonfinite"]))
            out["xl"] = [v if math.isfinite(v) else 0.0 for v in out["xl"]]
            out["xr"] = [v if math.isfinite(v) else 0.0 for v in out["xr"]]
        try:
            write_wav16(os.path.join(run, label + ".wav"), [out["xl"], out["xr"]])
        except Exception as ex:
            log("could not save the recording:", repr(ex))
        out["statlog"] = list(cl.L.statlog)
        out["t_flow"] = t_flow
        out["cl"] = cl
    finally:
        cl.stop()
    return out


def analyze_tone(x, hz=TONE_HZ):
    """Active span and steady-state continuity of a recorded steady tone."""
    ser = tone_series(x, hz, win=480, hop=48)
    if not ser:
        return None
    amps = sorted(a for _, a in ser)
    ref = amps[int(len(amps) * 0.75)]
    live = [i for i, a in ser if a > 0.5 * ref]
    if not live or ref < 0.005:
        return {"ref_amp": ref, "span_s": 0.0}
    on, off = live[0], live[-1]
    a0, a1 = on + RATE, off - RATE  # steady part: a second in from each end
    if a1 - a0 < 5 * RATE:
        return {"ref_amp": ref, "span_s": (off - on) / RATE}
    seg = x[a0:a1]
    step, step_at = max((abs(seg[i + 1] - seg[i]), i) for i in range(len(seg) - 1))
    # longest run of (near) digital silence: a pure tone is under 1 % of its amplitude for one sample
    # at a zero crossing, so a run of dozens of samples is a dropout
    quiet, run_len, silent_run = 0.01 * ref, 0, 0
    for v in seg:
        run_len = run_len + 1 if abs(v) < quiet else 0
        silent_run = max(silent_run, run_len)
    sa = [a for i, a in ser if a0 <= i < a1]
    # phase: two-cycle boxcar demodulation every 48 samples, minus its linear fit
    w = 2 * math.pi * hz / RATE
    L = int(round(2 * RATE / hz))
    cs, sn = [0.0], [0.0]
    for n, v in enumerate(seg):
        cs.append(cs[-1] + v * math.cos(w * n))
        sn.append(sn[-1] + v * math.sin(w * n))
    ph = []
    for n in range(0, len(seg) - L, 48):
        ph.append(math.atan2(sn[n + L] - sn[n], cs[n + L] - cs[n]))
    for i in range(1, len(ph)):  # unwrap
        d = ph[i] - ph[i - 1]
        ph[i] -= 2 * math.pi * round(d / (2 * math.pi))
    m = len(ph)
    xs = range(m)
    mx, my = (m - 1) / 2, sum(ph) / m
    sxx = sum((i - mx) ** 2 for i in xs)
    k = sum((i - mx) * (p - my) for i, p in zip(xs, ph)) / sxx
    dev = max(abs(p - (my + k * (i - mx))) for i, p in zip(xs, ph))
    hop_jump = max(abs(ph[i + 1] - ph[i]) for i in range(m - 1))
    return {"ref_amp": ref, "span_s": (off - on) / RATE, "max_step": step, "max_step_at_s": (a0 + step_at) / RATE, "max_silent_run": silent_run, "phase_dev_rad": dev,
            "phase_hop_jump_rad": hop_jump, "amp_min_ratio": min(sa) / ref if sa else None}


def tone_numbers(tr, i=0):
    """Receiver and talker counters of talker i from a tone_run."""
    v, t = tr["v"], tr["t"][i]
    r = v["peers"].get(tr["keys"][i], {}).get("receiver", {})
    sent = t["sent"]
    markers = rget(r, "markers", default=1)
    slots = sent["packets"] + sent.get("test_dropped", 0) - markers
    return {"r": r, "sent": sent, "markers": markers, "slots": slots,
            "out": rget(r, "output_samples", "neteq_output_samples", default=0),
            "plc": rget(r, "plc_samples", "concealed_samples", default=0),
            "accel": rget(r, "accelerate_samples", "removed_samples_for_acceleration", default=0),
            "preempt": rget(r, "preemptive_samples", "inserted_samples_for_deceleration", default=0),
            "late": rget(r, "late", "late_packets_discarded", default=None),
            "dup": r.get("duplicate"), "rej": r.get("rejected"), "reord": r.get("reordered_accepted"),
            "local_late": r.get("local_late")}


def clean_ref(args, res, audio):
    if "m" not in CLEAN_REF:
        tr = tone_run(args, res, audio, "loss", "tone-clean", [("tone:%g" % TONE_HZ, 0, [])], TONE_DUR * args.scale)
        CLEAN_REF["tr"] = tr
        CLEAN_REF["m"] = analyze_tone(tr["xl"])
        res.data.setdefault("loss", {})["clean"] = {"analysis": CLEAN_REF["m"], "counters": {k: v for k, v in tone_numbers(tr).items() if k != "r"}}
        n = tone_numbers(tr)
        res.oracle("loss", "L0", "lossless tone: every slot plays, none concealed, continuous (yardstick for the loss runs)",
                   bool(tr["flowing"]) and CLEAN_REF["m"] is not None and "max_step" in CLEAN_REF["m"] and n["plc"] == 0 and (n["late"] or 0) == 0
                   and n["out"] + n["accel"] - n["preempt"] == n["slots"] * 960 and tr["nonfinite"]["after_start"] == 0
                   and CLEAN_REF["m"].get("max_step", 9) <= 1.5 * lossless_step(CLEAN_REF["m"]) and CLEAN_REF["m"].get("max_silent_run", 9e9) <= SILENT_RUN_MAX,
                   "slots %s, output %s (+accel %s, -preempt %s), plc %s, late %s; %s; %s. A failure here with no listener underrun means the host disturbed the recording (host load), not the receiver: rerun on a quiet host" %
                   (n["slots"], n["out"], n["accel"], n["preempt"], n["plc"], n["late"], CLEAN_REF["m"], health(tr)))
    return CLEAN_REF["m"]


# Continuity of a recorded steady tone through concealment. Opus PLC does not keep the phase of a
# pure tone (a lost 20 ms is continued at a phase up to pi off, and the next good packet is
# cross-faded in), so the phase jump and the 10 ms amplitude dip are reported, not asserted. What
# is asserted is what a listener hears as a click or a hole: no hard splice (a cross-fade of two
# tones can at most roughly double to triple the tone's own maximum sample step (measured: up to
# 2.7x on the new receiver); a splice is ~30x, the old receiver's dropouts 5-9x) and no dropout (no
# run of near-silent samples longer than 0.5 ms).
STEP_RATIO_MAX = 4.0      # max sample step of a faulted run vs the lossless run
SILENT_RUN_MAX = 48       # samples under 1 % of the tone's amplitude in a row (1 ms). A concealed slot cross-fades two
#                           tones of unrelated phase and can null for 0.5-0.6 ms (seen: 26, 31); a dropout is 10-100x longer
CONCEAL_TOL_PCT = 0.35    # concealed_pct vs the scripted rate, in percentage points
SPAN_TOL_S = 0.080        # recorded tone duration vs the receiver's output samples: the ring (up to 3 blocks, 30 ms) and the sink's quantum still hold audio when the recorder stops, so the recording is 24-48 ms shorter


def lossless_step(ref):
    """Largest sample step of the pure tone at the amplitude the lossless run measured (2 pi f A / fs,
    +10 % for the 440 Hz peak landing between samples). Computed, not read from the clean recording, so
    a glitch in one recording (a starved recorder on a loaded host) cannot move every other oracle's
    yardstick; L0 asserts the clean recording agrees with it."""
    return 1.1 * 2 * math.pi * TONE_HZ / RATE * ref["ref_amp"]


def continuous(a, ref):
    return (a is not None and ref is not None and "max_step" in a and "ref_amp" in ref
            and a["max_step"] <= STEP_RATIO_MAX * lossless_step(ref) and a["max_silent_run"] <= SILENT_RUN_MAX)


def health(tr):
    """Listener-side facts that tell a glitch in the code from one in the recording path."""
    h = tr["v"]["histograms"]
    return "listener underruns %s total, callback gap max %s us, mixer block max %s us, host load1 %.1f" % (
        tr["v"]["underruns"], h["callback_gap_us"]["max"], h["mixer_block_us"]["max"], os.getloadavg()[0])


def cont_detail(a, ref):
    g = lambda d, k: None if not d or d.get(k) is None else round(d[k], 4)
    return ("max step %s (at %s s) vs lossless %s, longest near-silent run %s samples (lossless %s); info: amp dip min %s, phase hop jump %s rad (lossless %s)" %
            (g(a, "max_step"), g(a, "max_step_at_s"), None if not ref else round(lossless_step(ref), 4), a and a.get("max_silent_run"), ref and ref.get("max_silent_run"),
             g(a, "amp_min_ratio"), g(a, "phase_hop_jump_rad"), g(ref, "phase_hop_jump_rad")))


def loss_oracle(args, res, audio, label, extra, scripted_pct, scen="loss"):
    ref = clean_ref(args, res, audio)
    tr = tone_run(args, res, audio, scen, "tone-" + label, [("tone:%g" % TONE_HZ, 0, extra)], TONE_DUR * args.scale)
    n, a = tone_numbers(tr), analyze_tone(tr["xl"])
    t = tr["t"][0]
    dropped = t["sent"].get("test_dropped", 0)
    exact = n["out"] + n["accel"] - n["preempt"] == n["slots"] * 960
    pct = 100.0 * n["plc"] / max(1, n["out"])
    # Level control may stretch the tone (preemptive expand) by a few tens of ms in all; the recording
    # must last exactly as long as the receiver says it played.
    span_exp = n["out"] / RATE
    sid = "loss-" + label
    res.data.setdefault(scen, {})[label] = {"analysis": a, "counters": {k: v for k, v in n.items() if k != "r"}, "receiver": n["r"], "nonfinite": tr["nonfinite"], "concealed_pct": pct, "scripted_pct": scripted_pct}
    res.oracle(scen, sid + "-count", "%s: samples out = slots x 960 (no slot vanishes or appears)" % label, exact and n["slots"] > 0,
               "talker slots %d (sent %d + dropped %d - markers %d), receiver output %d + accelerate %d - preemptive %d = %d, want %d" %
               (n["slots"], n["sent"]["packets"], dropped, n["markers"], n["out"], n["accel"], n["preempt"], n["out"] + n["accel"] - n["preempt"], n["slots"] * 960))
    res.oracle(scen, sid + "-duration", "%s: the recorded tone lasts as long as the receiver's output, which is the input plus its logged stretch (%.0f ms)" % (label, SPAN_TOL_S * 1000),
               a is not None and abs(a["span_s"] - span_exp) <= SPAN_TOL_S,
               "recorded %.3f s, receiver output %.3f s (input %d slots = %.3f s, stretch +%d -%d samples)" % (a["span_s"] if a else -1, span_exp, n["slots"], n["slots"] * 0.02, n["preempt"], n["accel"]))
    res.oracle(scen, sid + "-concealed", "%s: concealed %% = scripted %.1f %% within %.2f pt; one concealed slot per lost packet" % (label, scripted_pct, CONCEAL_TOL_PCT),
               abs(pct - scripted_pct) <= CONCEAL_TOL_PCT and n["plc"] == dropped * 960,
               "plc %d samples (%.2f %% of output %d), dropped packets %d (x 960 = %d), fec %s" % (n["plc"], pct, n["out"], dropped, dropped * 960, n["r"].get("fec_samples")))
    cont = continuous(a, ref)
    res.oracle(scen, sid + "-continuity", "%s: the tone stays continuous through concealment (no splice: step <= %.1fx lossless; no dropout: no near-silent run > %d samples)" % (label, STEP_RATIO_MAX, SILENT_RUN_MAX), cont,
               cont_detail(a, ref) + "; " + health(tr))
    res.oracle(scen, sid + "-counters", "%s: late = duplicate = rejected = local_late = 0 (nothing but the scripted losses)" % label,
               (n["late"], n["dup"], n["rej"], n["local_late"]) == (0, 0, 0, 0) and tr["nonfinite"]["after_start"] == 0,
               "late %s, duplicate %s, rejected %s, local_late %s, decode_errors %s; non-finite samples in the recording after the first 0.1 s: %d (the recorder's first sample is NaN in every run, old receiver too); output underruns after warm-up %d" %
               (n["late"], n["dup"], n["rej"], n["local_late"], n["r"].get("decode_errors"), tr["nonfinite"]["after_start"], tr["v"]["underruns"] - tr["u0"]))


LOSS_RUNS = [("every50", ["--test-drop-every", "50"], 2.0), ("every20", ["--test-drop-every", "20"], 5.0),
             ("burst2of50", ["--test-drop-burst", "50,2"], 4.0)]


def scen_loss(args, res, audio):
    only = [x for x in (args.loss_only or "").split(",") if x]
    for label, extra, pct in LOSS_RUNS:
        for rep in range(max(1, args.repeat) if only else 1):
            if only and label not in only:
                continue
            loss_oracle(args, res, audio, label + ("" if rep == 0 else "-%d" % rep), extra, pct)


def scen_reorder(args, res, audio):
    scen = "reorder"
    ref = clean_ref(args, res, audio)
    # swap: every 20th packet goes out after its successor (20 ms late, inside the jitter buffer)
    tr = tone_run(args, res, audio, scen, "tone-swap", [("tone:%g" % TONE_HZ, 0, ["--test-reorder", "20,1"])], TONE_DUR * args.scale)
    n, a = tone_numbers(tr), analyze_tone(tr["xl"])
    held = tr["t"][0]["sent"].get("test_reordered", 0)
    cont = continuous(a, ref)
    res.data.setdefault(scen, {})["swap"] = {"analysis": a, "counters": {k: v for k, v in n.items() if k != "r"}, "held": held}
    res.oracle(scen, "ord-swap", "swapped neighbours are put back in order: reordered_accepted = swaps, none late, none concealed, tone continuous",
               held > 20 and n["reord"] is not None and abs(n["reord"] - held) <= 1 and n["late"] == 0 and n["plc"] == 0 and n["dup"] == 0 and cont,
               "talker swapped %d; receiver reordered_accepted %s, late %s, duplicate %s, plc %s; %s" %
               (held, n["reord"], n["late"], n["dup"], n["plc"], cont_detail(a, ref)))
    # late: every 25th packet is held 40 packets (0.8 s) and arrives long after its slot played
    tr = tone_run(args, res, audio, scen, "tone-late", [("tone:%g" % TONE_HZ, 0, ["--test-reorder", "25,40"])], TONE_DUR * args.scale)
    n, a = tone_numbers(tr), analyze_tone(tr["xl"])
    held = tr["t"][0]["sent"].get("test_reordered", 0)
    cont = continuous(a, ref)
    res.data[scen]["late"] = {"analysis": a, "counters": {k: v for k, v in n.items() if k != "r"}, "held": held}
    # plc_samples is counted after level control, so a stretched concealed slot is not exactly 960
    near = n["late"] is not None and abs(n["plc"] - n["late"] * 960) <= 0.1 * n["late"] * 960
    ok = (held > 20 and n["late"] is not None and held - 2 <= n["late"] + (n["reord"] or 0) <= held and near and n["dup"] == 0)
    # Continuity is reported, not asserted: a packet 0.8 s late is evidence the buffer was too shallow,
    # so the estimator (by design) raises the target from ~20 ms toward its 250 ms cap, and the buffer
    # builds by PreemptiveExpand time-stretching, which splices a pure tone (max step up to ~14x,
    # a 36-sample near-silent run). A single steady tone also cannot show old audio played after new;
    # that is proven by the exact counters here (late, one concealed slot each, no extra slot played)
    # and by the chirp ordering test in voice-core.
    res.oracle(scen, "ord-late", "a packet 0.8 s late is counted late and never played: late + reordered_accepted = held (+-2 at the end), one concealed slot per late packet (+-10 %), no duplicate (continuity of the stretched tone is reported only)",
               ok, "talker held %d; receiver late %s, reordered_accepted %s, duplicate %s, plc %s (late x 960 = %s); %s" %
               (held, n["late"], n["reord"], n["dup"], n["plc"], None if n["late"] is None else n["late"] * 960, cont_detail(a, ref)))


def scen_drift(args, res, audio):
    scen = "drift"
    dur = 90.0 * args.scale
    tr = tone_run(args, res, audio, scen, "nodtx", [("tone:%g" % TONE_HZ, 500, ["--no-dtx"]), ("tone:%g" % TONE_HZ, -500, ["--no-dtx"])], dur)
    t_flow = tr["t_flow"]
    for i, skew in enumerate((500, -500)):
        key, n = tr["keys"][i], tone_numbers(tr, i)
        pts = [(tt - t_flow, s["peers"].get(key, {}).get("receiver", {})) for tt, s in tr["statlog"]]
        pts = [(tt, r) for tt, r in pts if r and r.get("target_delay_ms") is not None]
        span = pts[-1][0] if pts else 0
        w = min(30, span / 3)

        def m(lo, hi, k):
            vs = [r[k] for tt, r in pts if lo <= tt <= hi and r.get(k) is not None]
            return sum(vs) / len(vs) if vs else None
        f_t, l_t = m(10, 10 + w, "target_delay_ms"), m(span - w, span, "target_delay_ms")
        f_b, l_b = m(10, 10 + w, "buffer_ms"), m(span - w, span, "buffer_ms")
        tmax = max((r["target_delay_ms"] for tt, r in pts if tt >= 10), default=None)
        tmin = min((r["target_delay_ms"] for tt, r in pts if tt >= 10), default=None)
        res.data.setdefault(scen, {})["T%d" % (i + 1)] = {"skew_ppm": skew, "target_first": f_t, "target_last": l_t, "buffer_first": f_b, "buffer_last": l_b,
                                                          "target_min": tmin, "target_max": tmax, "counters": {k: v for k, v in n.items() if k != "r"},
                                                          "accelerate_ops": n["r"].get("accelerate_ops"), "preemptive_ops": n["r"].get("preemptive_ops")}
        flat = None not in (f_t, l_t, f_b, l_b) and l_t <= f_t + 15 and l_b <= f_b + 15 and f_t - 15 <= l_t
        res.oracle(scen, "drift-%+d-flat" % skew, "%+d ppm talker, DTX off, %.0f s: target and buffer delay stay flat (within 15 ms of the first 30 s)" % (skew, dur), flat,
                   "target %.1f -> %.1f ms (range %s..%s), buffer %.1f -> %.1f ms, accelerate ops %s, preemptive ops %s" %
                   (f_t or -1, l_t or -1, tmin, tmax, f_b or -1, l_b or -1, n["r"].get("accelerate_ops"), n["r"].get("preemptive_ops")))
        res.oracle(scen, "drift-%+d-underruns" % skew, "%+d ppm talker: no underrun (no concealed sample, none late) and no output underrun" % skew,
                   n["plc"] == 0 and (n["late"] or 0) == 0 and tr["v"]["underruns"] - tr["u0"] == 0,
                   "plc %s samples, late %s, local_late %s, output underruns %d; slots %s, output %s + accel %s - preempt %s" %
                   (n["plc"], n["late"], n["local_late"], tr["v"]["underruns"] - tr["u0"], n["slots"], n["out"], n["accel"], n["preempt"]))


class ForceQuantum:
    """PipeWire `clock.force-quantum` for the duration of a run, restored afterwards."""

    def __init__(self, q):
        self.q, self.prev = q, None

    @staticmethod
    def read():
        md = sh(["pw-metadata", "-n", "settings"], check=False)
        m = re.search(r"key:'clock\.force-quantum' value:'(\d+)'", md)
        return m.group(1) if m else None

    def __enter__(self):
        self.prev = self.orig = self.read() or "0"
        CLEANUPS.append(self.restore)
        sh(["pw-metadata", "-n", "settings", "0", "clock.force-quantum", str(self.q)])
        time.sleep(0.5)
        self.applied = self.read()
        return self

    def restore(self):
        if self.prev is not None:
            sh(["pw-metadata", "-n", "settings", "0", "clock.force-quantum", self.prev], check=False)
            self.prev = None

    def __exit__(self, *a):
        self.restore()


def scen_quantum(args, res, audio):
    scen = "quantum"
    LISTENER_ENV["PIPEWIRE_LATENCY"] = "1024/48000"
    try:
        with ForceQuantum(1024) as fq:
            cl, rows, v, c, _ = run_load(args, res, audio, scen, False, 40 * args.scale)
            applied = fq.applied
    finally:
        LISTENER_ENV.pop("PIPEWIRE_LATENCY", None)
    h = v["histograms"]["callback_gap_us"]
    after = ForceQuantum.read()
    burst = (v.get("mixer") or {}).get("out_burst_frames")
    ahead = (v.get("mixer") or {}).get("ring_ahead_frames")
    # ALSA delivers a 1024 quantum as two 512 callbacks back to back, so the gap histogram is
    # bimodal (p50 under 64 us, max ~21 ms); the pull size is what the receiver measures itself.
    res.oracle(scen, "q-applied", "clock.force-quantum was 1024 during the run (setting restored afterwards) and the device pulled >= 1024 frames per burst, so the mixer kept > 1024 frames ahead",
               applied == "1024" and after == fq.orig and burst is not None and burst >= 1000 and ahead is not None and ahead > burst,
               "force-quantum during %s, after %s; largest pull burst %s frames, ring kept %s frames ahead; callback gap max %s us, n %s" % (applied, after, burst, ahead, h["max"], h["count"]))
    res.oracle(scen, "q-report", "quantum 1024: underruns, listener age and concealment (info)", None,
               {"underruns": v["underruns"], "listener_age_us": hist_row(v["histograms"]["listener_age_us"]),
                "buffer_ms_mean": {k: round(d.get("buffer_ms_mean") or 0, 1) for k, d in res.data[scen].items() if isinstance(d, dict) and "accepted" in d},
                "plc_pct": {k: round(d["concealed_active_pct"], 3) for k, d in res.data[scen].items() if isinstance(d, dict) and "accepted" in d}})


def paused_clips(args, res):
    """Seven looping WAVs where the talkers take turns: talker i speaks its clip in window i % 3 of a
    three-window cycle and is silent in the others (about one third of the time, at most three at once)."""
    clips = [read_mono_wav(c) for c in args.clips]
    win = max(len(c) for c in clips) + RATE // 2
    outd = os.path.join(res.run, "paused-clips")
    os.makedirs(outd, exist_ok=True)
    paths = []
    for i in range(7):
        c = clips[CLIP_ORDER[i] % len(clips)]
        w = array.array("h", [0] * (3 * win))
        o = (i % 3) * win
        w[o:o + len(c)] = c
        pth = os.path.join(outd, "talker%d.wav" % i)
        f = wave.open(pth, "wb")
        f.setnchannels(1)
        f.setsampwidth(2)
        f.setframerate(RATE)
        f.writeframes(w.tobytes())
        f.close()
        paths.append(pth)
    return paths


def scen_idle(args, res, audio):
    """Silent peers: speech with pauses (talkers take turns), idle skip on and off, then the
    continuous-talk case (all seven always speaking) with the skip on."""
    global CLIP_ORDER
    scen = "idle"
    out = {}
    saved_clips, saved_order = args.clips, CLIP_ORDER
    try:
        args.clips = paused_clips(args, res)
        CLIP_ORDER = list(range(7))
        for label, extra in (("skip", []), ("noskip", ["--test-no-idle-skip"])):
            LISTENER_EXTRA[:] = extra
            try:
                cl, rows, v, c, _ = run_load(args, res, audio, scen + "-" + label, False, 60 * args.scale, idle_ok=True)
            finally:
                LISTENER_EXTRA[:] = []
            out[label] = idle_row(res, scen + "-" + label, v)
    finally:
        args.clips, CLIP_ORDER = saved_clips, saved_order
    cl, rows, v, c, _ = run_load(args, res, audio, scen + "-continuous", False, 60 * args.scale)
    out["continuous"] = idle_row(res, scen + "-continuous", v)
    a, b, k = out["skip"], out["noskip"], out["continuous"]
    res.data[scen] = out
    tot = lambda r: (r["skipped"] or 0) + (r["rendered"] or 0) + (r["tail"] or 0)
    share = (a["skipped"] or 0) / max(1, tot(a))
    kshare = (k["skipped"] or 0) / max(1, tot(k))
    f = lambda x: None if x is None else round(x, 2)
    res.oracle(scen, "idle-skip", "speech with pauses: >= 30 % of peer-blocks are skipped, none with the skip off, the skip lowers the listener's CPU and mixer block p99, no underruns",
               share >= 0.3 and not b["skipped"] and a["cpu_pct"] is not None and b["cpu_pct"] is not None and a["cpu_pct"] < b["cpu_pct"]
               and a["underruns"] == 0 and b["underruns"] == 0,
               "skip on: cpu %s %% of a core, skipped %.0f %% of %d peer-blocks, block p99 %s us, p99.9 %s us, max %s us, underruns %s; skip off: cpu %s %%, block p99 %s us, p99.9 %s us, max %s us, underruns %s" %
               (f(a["cpu_pct"]), 100 * share, tot(a), a["stages"]["block"]["p99"], a["stages"]["block"]["p99_9"], a["stages"]["block"]["max"], a["underruns"],
                f(b["cpu_pct"]), b["stages"]["block"]["p99"], b["stages"]["block"]["p99_9"], b["stages"]["block"]["max"], b["underruns"]))
    res.oracle(scen, "idle-continuous", "continuous talk (7 talkers always speaking): little is skipped and nothing underruns (info: the skip gives nothing here)",
               k["underruns"] == 0,
               "skipped %.1f %% of %d peer-blocks, cpu %s %% of a core, block p99 %s us, p99.9 %s us, max %s us, underruns %s" %
               (100 * kshare, tot(k), f(k["cpu_pct"]), k["stages"]["block"]["p99"], k["stages"]["block"]["p99_9"], k["stages"]["block"]["max"], k["underruns"]))


def idle_row(res, name, v):
    d = res.data[name]
    mx = v.get("mixer") or {}
    return {"cpu_pct": d.get("listener_cpu_pct_of_core"), "stages": d["mixer"]["stages_us_per_block"],
            "rendered": mx.get("peer_blocks_rendered"), "tail": mx.get("peer_blocks_tail"), "skipped": mx.get("peer_blocks_skipped"),
            "underruns": v["underruns"], "listener_age_us": hist_row(v["histograms"]["listener_age_us"])}


# ---------------------------------------------------------------- main
def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("scenario", choices=["baseline", "bulk", "listener", "faults", "stale", "scenes", "loss", "reorder", "drift", "quantum", "idle", "all"])
    ap.add_argument("--prim", default=os.path.join(ROOT, "target/release/prim"))
    ap.add_argument("--out", default=os.path.join(ROOT, ".local/r0-runs"))
    ap.add_argument("--scale", type=float, default=1.0, help="scale the long durations (debug)")
    ap.add_argument("--godot", default=GODOT_DEF)
    ap.add_argument("--clips", default=None)
    ap.add_argument("--skews", default=None, help="comma list of 7 ppm values overriding SKEWS (attribution runs)")
    ap.add_argument("--clip-order", default=None, help="comma list of 7 clip indexes (0-3) overriding i%%4 (attribution runs)")
    ap.add_argument("--loss-only", default="", help="debug: run only these loss runs (comma labels: every50,every20,burst2of50)")
    ap.add_argument("--repeat", type=int, default=1, help="debug: with --loss-only, repeat each run N times")
    ap.add_argument("--listener-prim", default=None, help="run the listener (node L) voice process from this build, talkers from --prim (proves the new talker faults fail an old receiver)")
    ap.add_argument("--drop-every", type=int, default=0, help="TEST ONLY: talkers drop every Nth media packet (proves 1b can fail)")
    args = ap.parse_args()
    global SKEWS, CLIP_ORDER, DROP_EVERY
    if args.skews:
        SKEWS = [int(x) for x in args.skews.split(",")]
        assert len(SKEWS) == 7
    if args.clip_order:
        CLIP_ORDER = [int(x) for x in args.clip_order.split(",")]
        assert len(CLIP_ORDER) == 7
    DROP_EVERY = args.drop_every
    global LISTENER_PRIM
    LISTENER_PRIM = args.listener_prim
    os.makedirs(args.out, exist_ok=True)
    tagp = os.path.join(args.out, "CACHEDIR.TAG")
    if not os.path.exists(tagp):
        open(tagp, "w").write(TAG + "# regenerable r0 oracle runs\n")
    clipdir = os.path.join(args.godot, "inputs")
    args.clips = [os.path.join(clipdir, n + ".wav") for n in
                  ("6930-75918-0000", "1320-122617-0003", "5639-40744-0002", "260-123440-0010")]
    run = os.path.join(args.out, datetime.datetime.now().strftime("%Y%m%d-%H%M%S-") + args.scenario)
    os.makedirs(run)
    res = Results(run)
    audio = Audio()
    env = environment(args.prim)
    env["default_audio_before"] = audio.default_before
    log("run dir", run)
    todo = ["baseline", "bulk", "listener", "faults", "stale", "scenes", "loss", "reorder", "drift", "quantum", "idle"] if args.scenario == "all" else [args.scenario]
    base = bulk = None
    try:
        for s in todo:
            log("=== scenario", s)
            n_tallies = len(CLUSTER_TALLIES)
            try:
                if s == "baseline":
                    base = scen_baseline(args, res, audio)
                elif s == "bulk":
                    bulk = scen_bulk(args, res, audio, base)
                elif s == "listener":
                    scen_listener(args, res, audio, base[1] if base else None, bulk[1] if bulk else None)
                elif s == "faults":
                    scen_faults(args, res, audio)
                elif s == "stale":
                    scen_stale(args, res, audio)
                elif s == "scenes":
                    scen_scenes(args, res, audio)
                elif s == "loss":
                    scen_loss(args, res, audio)
                elif s == "reorder":
                    scen_reorder(args, res, audio)
                elif s == "drift":
                    scen_drift(args, res, audio)
                elif s == "quantum":
                    scen_quantum(args, res, audio)
                elif s == "idle":
                    scen_idle(args, res, audio)
            except Exception as e:
                import traceback
                traceback.print_exc()
                res.oracle(s, "error", "scenario aborted", False, repr(e))
            if s not in ("scenes", "loss", "reorder", "drift", "quantum") and len(CLUSTER_TALLIES) > n_tallies:  # listener reuses earlier clusters
                lane_oracle(res, s, CLUSTER_TALLIES[n_tallies:])
            res.save(env)
    finally:
        cleanup_all()
        after = Audio.default_sink()
        for k, d in res.data.items():  # which codec and receiver the listener really ran
            if isinstance(d, dict) and d.get("opus_version"):
                env["opus_version"] = d["opus_version"]
                break
        env["receiver"] = ("timeline (PlayoutReceiver)" if any(isinstance(d, dict) and any(isinstance(x, dict) and x.get("plc_samples") is not None for x in d.values()) for d in res.data.values()) else "unknown or NetEq")
        env["default_audio_after"] = after
        env["default_audio_unchanged"] = after == audio.default_before
        env["safety_violation"] = audio.violation
        res.save(env)
    log("summary:", os.path.join(run, "summary.md"))
    bad = [i for i in res.items if i["pass"] is False]
    log("%d oracles, %d failed" % (len(res.items), len(bad)))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
