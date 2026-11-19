# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy"]
# ///
"""Build the blind listening packets (offline; nothing is ever played).

  uv run tools/listen/build_packets.py packet1 --work DIR --out DIR --old-bin PATH --new-bin PATH CLIP.f32 CLIP.f32
  uv run tools/listen/build_packets.py packet2 --work DIR --out DIR --new-bin PATH WAV48 WAV48 WAV48

Inputs are mono 48 kHz f32le files (convert with ffmpeg first). --work is regenerable scratch
(a CACHEDIR.TAG is written there). --out gets the listening files, the page-ready webm-opus
files, and the KEY-open-only-after-answering.json (answers to which file is which).
"""
import argparse, json, math, os, random, subprocess, sys
from concurrent.futures import ThreadPoolExecutor
import numpy as np

SR = 48000
TAG = "Signature: 8a477f597d28d172789f06886806bc55\n# regenerable listening-packet scratch\n"
TARGET_DBFS = -26.0
FADE = int(0.050 * SR)


def sh(cmd, **kw):
    return subprocess.run(cmd, check=True, **kw)


def rd(path):
    return np.fromfile(path, dtype="<f4").astype(np.float64)


def trim_start(x):
    nz = np.flatnonzero(np.abs(x) > 1e-6)
    return x[nz[0]:] if len(nz) else x


def align_to_source(y, src, max_ms=1000):
    """Drop the receiver's start-up latency: find the lag (1 ms steps, 0..max_ms) at which the
    envelope of y best matches the source, return y from there and the lag in ms. Both receivers of
    a pair are aligned to the same source, so neither carries an offset tell."""
    env = lambda v: np.sqrt(np.convolve(v * v, np.ones(480) / 480, "valid")[::48])
    a = env(src)
    b = env(y[: len(src) + max_ms * 48 + 480])
    best, best_c = 0, -2.0
    for lag in range(0, max_ms):
        n = min(len(a), len(b) - lag)
        if n < len(a) // 2:
            break
        c = float(np.corrcoef(a[:n], b[lag:lag + n])[0, 1])
        if c > best_c:
            best, best_c = lag, c
    return y[best * 48:], best


def rms(x):
    return float(np.sqrt(np.mean(x * x)))


def fade(x):
    x = x.copy()
    r = np.linspace(0, 1, FADE)
    x[:FADE] *= r
    x[-FADE:] *= r[::-1]
    return x


def write_outputs(x, base):
    """x float64 mono 48k -> base.wav (scratch), base.flac, base.webm (opus 96 kb/s)."""
    pcm = (np.clip(x, -1, 1) * 32767).astype("<i2")
    wav = base + ".wav"
    import wave
    with wave.open(wav, "wb") as w:
        w.setnchannels(1); w.setsampwidth(2); w.setframerate(SR); w.writeframes(pcm.tobytes())
    sh(["ffmpeg", "-nostdin", "-v", "error", "-y", "-i", wav, "-c:a", "flac", base + ".flac"])
    sh(["ffmpeg", "-nostdin", "-v", "error", "-y", "-i", wav, "-c:a", "libopus", "-b:a", "96k",
        "-application", "audio", "-vbr", "on", base + ".webm"])
    os.remove(wav)


def level(x, target_db=TARGET_DBFS):
    g = 10 ** (target_db / 20) / rms(x)
    return x * g, g


def packet1(a):
    os.makedirs(a.work, exist_ok=True); os.makedirs(a.out, exist_ok=True)
    open(os.path.join(a.work, "CACHEDIR.TAG"), "w").write(TAG)
    conds = [("noloss", 0.0), ("loss2", 2.0), ("loss5", 5.0)]
    jobs = []  # (clip_idx, cond, events, newf32, oldf32, stats paths)
    for ci, clip in enumerate(a.clips):
        for cname, pct in conds:
            base = os.path.join(a.work, f"c{ci}-{cname}")
            seed = 1000 + 17 * ci + int(pct)
            sh([a.new_bin, "pkts", clip, base + ".events", "16", str(pct), str(seed)])
            jobs.append(dict(ci=ci, cond=cname, pct=pct, seed=seed, base=base))
    # new receiver is instant; old receiver is real time: old runs are real time
    for j in jobs:
        sh([a.new_bin, "play", j["base"] + ".events", j["base"] + ".new.f32", j["base"] + ".new.json"])
    # at most two at a time (each is real time but light); callers run the whole thing under nice
    for k in range(0, len(jobs), 2):
        procs = [subprocess.Popen([a.old_bin, j["base"] + ".events", j["base"] + ".old.f32", j["base"] + ".old.json"]) for j in jobs[k:k + 2]]
        for p in procs:
            assert p.wait() == 0
    rng = random.Random(20261118)
    orders = ["old_is_A"] * 3 + ["new_is_A"] * 3  # balanced, shuffled by the seed
    rng.shuffle(orders)
    key = {"note": "Open only after answering. A/B order was randomised per pair.", "pairs": []}
    pairs = []
    n = 0
    for j in jobs:
        n += 1
        srcx = rd(a.clips[j["ci"]])
        dur_sec = srcx.size * (1.0 / SR)
        old, lag_old = align_to_source(rd(j["base"] + ".old.f32"), srcx)
        new, lag_new = align_to_source(rd(j["base"] + ".new.f32"), srcx)
        keep = int((dur_sec + 0.5) * SR)
        L = min(len(old), len(new), keep)
        old, new = fade(old[:L]), fade(new[:L])
        r_old, r_new = rms(old), rms(new)
        old_l, g_old = level(old); new_l, g_new = level(new)
        pid = f"P{n}"
        ab = orders[n - 1]
        A, B = (old_l, new_l) if ab == "old_is_A" else (new_l, old_l)
        for tag, x in (("A", A), ("B", B)):
            write_outputs(x, os.path.join(a.out, f"{pid}-{tag}"))
        meta = json.load(open(j["base"] + ".events.meta.json"))
        key["pairs"].append(dict(
            pair=pid, order=ab, clip=os.path.basename(a.clips[j["ci"]]), condition=j["cond"],
            loss_pattern=meta, old_stats=json.load(open(j["base"] + ".old.json")),
            new_stats=json.load(open(j["base"] + ".new.json")),
            samples_compared=L, align_lag_ms={"old": lag_old, "new": lag_new}, rms_before_match={"old": r_old, "new": r_new},
            match_gain_db={"old": 20 * math.log10(g_old), "new": 20 * math.log10(g_new)},
            peak_after={"old": float(np.abs(old_l).max()), "new": float(np.abs(new_l).max())},
            max_abs_diff_old_new=float(np.abs(old_l - new_l).max())))
        pairs.append(pid)
    json.dump(key, open(os.path.join(a.out, "KEY-open-only-after-answering.json"), "w"), indent=1)
    print(json.dumps(pairs))


def packet2(a):
    os.makedirs(a.work, exist_ok=True); os.makedirs(a.out, exist_ok=True)
    open(os.path.join(a.work, "CACHEDIR.TAG"), "w").write(TAG)
    rng = random.Random(20261119)
    key = {"note": "Open only after answering. Labels X1..X9 are blind; trios are clips 1-3.", "items": []}
    lab = 0
    for ci, wav in enumerate(a.clips):
        f32 = os.path.join(a.work, f"s{ci}.f32")
        sh(["ffmpeg", "-nostdin", "-v", "error", "-y", "-i", wav, "-ac", "1", "-ar", "48000", "-f", "f32le", f32])
        rates = [16, 24, 32]
        rng.shuffle(rates)
        outs = []
        for kb in rates:
            b = os.path.join(a.work, f"s{ci}-{kb}")
            sh([a.new_bin, "codec", f32, b + ".f32", str(kb), b + ".json"])
            y, lag = align_to_source(rd(b + ".f32"), rd(f32))
            outs.append((kb, y, json.load(open(b + ".json")), lag))
        L = min(len(o[1]) for o in outs)
        keep = int((os.path.getsize(f32) / 4 / SR + 0.3) * SR)
        L = min(L, keep)
        for kb, x, st, lag in outs:
            lab += 1
            y, g = level(fade(x[:L]))
            write_outputs(y, os.path.join(a.out, f"X{lab}"))
            key["items"].append(dict(label=f"X{lab}", clip=ci + 1, source=os.path.basename(wav), kbps=kb,
                                     match_gain_db=20 * math.log10(g), encode_stats=st, align_lag_ms=lag))
    json.dump(key, open(os.path.join(a.out, "KEY-open-only-after-answering.json"), "w"), indent=1)


if __name__ == "__main__":
    p = argparse.ArgumentParser()
    p.add_argument("which", choices=["packet1", "packet2"])
    p.add_argument("--work", required=True); p.add_argument("--out", required=True)
    p.add_argument("--old-bin"); p.add_argument("--new-bin", required=True)
    p.add_argument("clips", nargs="+")
    a = p.parse_args()
    {"packet1": packet1, "packet2": packet2}[a.which](a)
