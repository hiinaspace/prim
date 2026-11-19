#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["numpy", "opencv-python-headless"]
# ///
"""Smallest readable cell on REAL captures: downscale each saved crop (area filter, like a smaller eye buffer) and decode.
usage: x0_sweep.py RUNDIR ROLE EYE   -> JSON rows {scale, screen_px, cell_px, rate, wrong}"""
import json, sys, pathlib, importlib.util
import numpy as np, cv2
here = pathlib.Path(__file__).parent
s = importlib.util.spec_from_file_location("bd", here / "blockclock_decode.py"); bd = importlib.util.module_from_spec(s); s.loader.exec_module(bd)
run = pathlib.Path(sys.argv[1]); role = sys.argv[2]; eye = sys.argv[3]
rows = [r for r in json.load(open(run / f"{role}-decoded.json")) if r["eye"] == eye and r["ok"]]
ref = {r["file"]: r["frame"] for r in rows}
out = []
for scale in [1, .75, .5, .35, .25, .18, .12, .09, .07, .055, .045, .035]:
    ok = wrong = n = 0; w = None
    for r in rows[::2]:
        im = cv2.imread(str(run / r["file"]))
        im = cv2.resize(im, None, fx=scale, fy=scale, interpolation=cv2.INTER_AREA) if scale != 1 else im
        d = bd.decode_image(cv2.cvtColor(im, cv2.COLOR_BGR2RGB)); n += 1
        if d["ok"]:
            if d["frame"] == ref[r["file"]]: ok += 1
            else: wrong += 1
        if w is None and d.get("quad"):
            q = np.array(d["quad"]); w = float(np.linalg.norm(q[1] - q[0]))
    out.append({"scale": scale, "screen_w_px": w, "cell_px": (w / 6 if w else None), "rate": ok / n, "wrong": wrong, "n": n})
print(json.dumps(out))
