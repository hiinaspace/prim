#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["numpy", "opencv-python-headless", "pillow"]
# ///
"""Decode the frame number of a block-clock frame (tools/blockclock.py) from a
capture of the theater screen (per-eye crop or desktop screenshot), and find
audio ticks in a recording.

Library:
    from blockclock_decode import decode_image, decode_file, find_ticks
    r = decode_image(rgb_or_gray_array)   # {"ok": bool, "frame": int|None, "why": str, "contrast":..., "margin":...}
CLI:
    blockclock_decode.py image.png [...]            # prints one JSON line per file
    blockclock_decode.py --ticks rec.wav [--rate 48000]   # tick times (s from start), kind small/big
"""
import json, sys
import numpy as np
try:
    import cv2
except ImportError:  # audio-only use (find_ticks) needs numpy alone
    cv2 = None

W, H = 1280, 720
COLS, ROWS = 6, 5
NBITS = 20


def _ungray(g):
    n = 0
    while g:
        n ^= g
        g >>= 1
    return n


def _check4(g):
    x = 0
    for i in range(5):
        x ^= (g >> (4 * i)) & 0xF
    return x ^ 0xA


def _gray(img):
    a = np.asarray(img)
    if a.dtype != np.uint8:
        a = np.clip(a, 0, 1) if a.dtype.kind == "f" else a
        a = (a * 255).astype(np.uint8) if np.asarray(img).dtype.kind == "f" else a.astype(np.uint8)
    if a.ndim == 3:
        a = cv2.cvtColor(a[..., :3], cv2.COLOR_RGB2GRAY)
    return a


def _quad(g, frac=0.8):
    """Corners of the white border ring: largest bright component's hull as 4 points."""
    lo, hi = np.percentile(g, 5), np.percentile(g, 99.5)
    if hi - lo < 25:
        return None, "low contrast"
    t = lo + frac * (hi - lo)  # screen white is the brightest thing; sky and reflections sit below this
    bw = (g > t).astype(np.uint8)
    n, lab, stats, _ = cv2.connectedComponentsWithStats(bw, connectivity=4)
    if n < 2:
        return None, "no bright component"
    # the ring is the component with the largest bounding box (cells inside are smaller)
    i = 1 + int(np.argmax(stats[1:, cv2.CC_STAT_WIDTH] * stats[1:, cv2.CC_STAT_HEIGHT]))
    if stats[i, cv2.CC_STAT_WIDTH] < 24 or stats[i, cv2.CC_STAT_HEIGHT] < 14:
        return None, "screen too small"
    ys, xs = np.nonzero(lab == i)
    pts = np.stack([xs, ys], 1).astype(np.float32)
    hull = cv2.convexHull(pts)
    peri = cv2.arcLength(hull, True)
    quad = None
    for eps in np.linspace(0.01, 0.1, 19):
        ap = cv2.approxPolyDP(hull, eps * peri, True)
        if len(ap) == 4:
            quad = ap[:, 0, :]
            break
    if quad is None:
        box = cv2.boxPoints(cv2.minAreaRect(hull))
        quad = box
    # order TL, TR, BR, BL
    quad = np.array(quad, np.float32)
    s = quad.sum(1); d = quad[:, 0] - quad[:, 1]
    o = np.array([quad[np.argmin(s)], quad[np.argmax(d)], quad[np.argmax(s)], quad[np.argmin(d)]], np.float32)
    # pixel centres: hull of pixel centres underestimates the outer edge by ~0.5 px
    c = o.mean(0)
    o = c + (o - c) * (1 + 1.0 / max(np.linalg.norm(o[1] - o[0]), 8))
    return o, ""


def decode_image(img, cell=160) -> dict:
    # Sky/reflections can be bright, blur can dim the thin border: try strict, then looser, thresholds.
    g = _gray(img)
    last = {"ok": False, "frame": None, "why": "not tried"}
    for frac in (0.8, 0.65, 0.5):
        last = _decode(g, cell, frac)
        if last["ok"]:
            break
    return last


def _decode(g, cell, frac):
    quad, why = _quad(g, frac)
    if quad is None:
        return {"ok": False, "frame": None, "why": why}
    ch = cell * 7 // 10
    x0, y0 = (W - COLS * cell) // 2, (H - ROWS * ch) // 2
    best = None
    for rot in range(4):
        src = np.roll(quad, -rot, axis=0)
        dst = np.array([[0, 0], [W, 0], [W, H], [0, H]], np.float32) / 4  # 320x180 canvas
        M = cv2.getPerspectiveTransform(src, dst)
        # rot 0/2 keep landscape; 1/3 swap axes -> use aspect check to skip
        w = cv2.warpPerspective(g, M, (W // 4, H // 4), flags=cv2.INTER_AREA).astype(np.float32)

        def samp(r, c):
            cx = (x0 + (c + 0.5) * cell) / 4
            cy = (y0 + (r + 0.5) * ch) / 4
            hw, hh = cell / 4 * 0.3, ch / 4 * 0.3
            return float(w[int(cy - hh):int(cy + hh) + 1, int(cx - hw):int(cx + hw) + 1].mean())

        cal = [samp(0, c) for c in range(COLS)]
        wh = np.mean(cal[0::2]); bl = np.mean(cal[1::2])
        contrast = wh - bl
        # orientation sentinel: filled TL square vs the others' corners
        if contrast < 25:
            continue
        th = (wh + bl) / 2
        if not all((cal[c] > th) == (c % 2 == 0) for c in range(COLS)):
            continue
        vals = [samp(1 + i // COLS, i % COLS) for i in range(24)]
        bits = [1 if v > th else 0 for v in vals]
        gv = 0
        for b in bits[:NBITS]:
            gv = (gv << 1) | b
        cv = 0
        for b in bits[NBITS:]:
            cv = (cv << 1) | b
        margin = float(min(abs(v - th) for v in vals) / contrast)
        if margin < 0.2:  # a cell sits near the threshold: blur or sub-pixel aliasing, don't guess
            best = best or {"ok": False, "frame": None, "why": "ambiguous cell", "contrast": float(contrast)}
            continue
        if cv != _check4(gv):
            best = best or {"ok": False, "frame": None, "why": "check bits fail", "contrast": float(contrast)}
            continue
        return {"ok": True, "frame": _ungray(gv), "why": "", "contrast": float(contrast),
                "margin": margin, "rot": rot, "quad": quad.tolist()}
    return best or {"ok": False, "frame": None, "why": "grid not recognised"}


def decode_file(path):
    im = cv2.imread(path, cv2.IMREAD_COLOR)
    if im is None:
        return {"ok": False, "frame": None, "why": "unreadable file"}
    return decode_image(cv2.cvtColor(im, cv2.COLOR_BGR2RGB))


def find_ticks(samples, rate=48000):
    """Tick onsets (seconds from sample 0) in mono float audio; kind 'big' = 2 kHz/60 ms, 'small' = 1 kHz/20 ms.

    Onset = first time the band envelope crosses 25% of the tick's local peak, refined to the sample."""
    x = np.asarray(samples, np.float32)
    n = len(x)
    out = []
    # goertzel-ish band energy via short-window correlation with 1k and 2k carriers
    win = int(rate * 0.004)
    t = np.arange(n) / rate

    def band(f):
        c = x * np.cos(2 * np.pi * f * t); s = x * np.sin(2 * np.pi * f * t)
        k = np.ones(win, np.float32) / win
        return np.sqrt(np.convolve(c, k, "same") ** 2 + np.convolve(s, k, "same") ** 2)

    e1, e2 = band(1000.0), band(2000.0)
    e = np.maximum(e1, e2)
    pk = e.max()
    if pk <= 0:
        return out
    thr = 0.25 * pk
    above = e > thr
    i = 0
    refractory = int(rate * 0.6)  # ticks are 1 s apart; a clipped first tick can split in two
    while i < n:
        if above[i]:
            j = i
            while j < n and above[j]:
                j += 1
            dur = (j - i) / rate
            kind = "big" if e2[i:j].mean() > e1[i:j].mean() else "small"
            out.append({"t": i / rate, "dur": dur, "kind": kind})
            i = j + refractory
        else:
            i += 1
    return out


def main():
    a = sys.argv[1:]
    if a and a[0] == "--ticks":
        import wave
        rate = 48000
        with wave.open(a[1]) as w:
            rate = w.getframerate(); ch = w.getnchannels()
            d = np.frombuffer(w.readframes(w.getnframes()), "<i2").astype(np.float32) / 32768
        d = d.reshape(-1, ch).mean(1)
        print(json.dumps(find_ticks(d, rate)))
        return
    for p in a:
        r = decode_file(p); r.pop("quad", None); r["file"] = p
        print(json.dumps(r))


if __name__ == "__main__":
    main()
