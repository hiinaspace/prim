#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["numpy", "opencv-python-headless"]
# ///
"""Place one instance's picture clock and audio clock on the shared unix timeline.
usage: x0_timing.py RUNDIR ROLE [--fps 30] [--eye L|R|D]
Picture: capture at unix t shows frame f  =>  video zero t0 in [t-(f+1)/fps, t-f/fps]; intersect over captures.
Audio:   tick k (k seconds into the stream) heard at unix u_k => audio zero u_k - k."""
import json, sys, pathlib, importlib.util
import numpy as np
here = pathlib.Path(__file__).parent

def pic_zero(rows, fps, eye):
    rs = [r for r in rows if r["eye"] == eye and r["ok"] and r["kind"] != "paused" and r["frame"] >= 3]  # frame 0-2 may be the still-paused picture
    t = np.array([r["t_unix"] for r in rs]); f = np.array([r["frame"] for r in rs])
    T = t.min(); t = t - T
    lo = (t - (f + 1) / fps); hi = (t - f / fps)
    est = (t - (f + 0.5) / fps)
    # robust: drop gross outliers (decode errors / seek) using median
    med = np.median(est); keep = np.abs(est - med) < 0.5
    t, f, lo, hi, est = t[keep], f[keep], lo[keep], hi[keep], est[keep]
    A = np.vstack([t, np.ones_like(t)]).T
    slope, icpt = np.linalg.lstsq(A, f / fps, rcond=None)[0]
    res = f / fps - (slope * t + icpt)
    return {"n": int(len(t)), "dropped": int((~keep).sum()), "t0_mean": float(est.mean() + T), "t0_std_single": float(est.std()),
            "t0_sem": float(est.std() / np.sqrt(len(est))), "bound_lo": float(lo.max() + T), "bound_hi": float(hi.min() + T),
            "bound_consistent": bool(lo.max() <= hi.min()), "slope": float(slope), "resid_std_ms": float(res.std() * 1e3),
            "first_t": float(T), "last_t": float(t.max() + T), "span_s": float(t.max())}

def unpause_unix(run):
    for f in run.glob("*-caps.jsonl"):
        for line in open(f):
            j = json.loads(line)
            if j.get("kind") == "unpause": return j["t_unix"]
    return None


def aud_zero(run, prefix="audio"):
    s = importlib.util.spec_from_file_location("ba", here / "blockclock_audio.py"); m = importlib.util.module_from_spec(s); s.loader.exec_module(m)
    r = m.ticks(str(run / prefix))
    ticks = r["ticks"]
    after = unpause_unix(run)  # the player is audible for a moment before the harness pauses and rewinds; ignore that
    if after is not None: ticks = [x for x in ticks if x["unix"] >= after]
    while ticks and ticks[0]["kind"] != "big": ticks = ticks[1:]
    if not ticks: return None
    # tick k: nearest integer seconds from the first big tick (stream starts at the loop's start; first big tick = k multiple of 10)
    u = np.array([x["unix"] for x in ticks])
    # first detected tick index unknown (player may start mid-stream); take the first big tick as k%10==0 anchor
    # and spacing-check: consecutive ticks must be ~1.000 s apart
    d = np.diff(u)
    k = np.round(u - u[0]).astype(int)  # tick index relative to the first
    slope, icpt = np.polyfit(k, u - u[0], 1)
    # the first tick can be clipped by the audio start; zero from ticks k=1..5 after it (k = whole seconds since the first, a big tick at stream 0)
    sel = [(float(x), int(kk)) for x, kk in zip(u, k) if 1 <= kk <= 5]
    zero5 = float(np.mean([x - kk for x, kk in sel])) if sel else float(u[0])
    return {"zero5": zero5, "n": len(ticks), "first_unix": float(u[0]), "spacing_mean": float(d.mean()), "audio_clock_slope": float(slope), "audio_fit_resid_ms": float(np.std(u - u[0] - (slope * k + icpt)) * 1e3), "spacing_std_ms": float(d.std() * 1e3),
            "err_s": r["err_s"], "kinds": "".join("B" if x["kind"] == "big" else "s" for x in ticks[:25]), "ticks": ticks}

if __name__ == "__main__":
    run = pathlib.Path(sys.argv[1]); role = sys.argv[2]
    fps = int(sys.argv[sys.argv.index("--fps") + 1]) if "--fps" in sys.argv else 30
    rows = json.load(open(run / f"{role}-decoded.json"))
    out = {}
    for eye in sorted({r["eye"] for r in rows}):
        out[eye] = pic_zero(rows, fps, eye)
    a = aud_zero(run)
    if a: a.pop("ticks"); out["audio"] = a
    print(json.dumps(out, indent=1))
