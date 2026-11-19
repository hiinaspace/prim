#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["numpy", "opencv-python-headless"]
# ///
"""Inter-instance offset from two runs' pixels. usage: x0_pair.py RUNS_DIR LABEL OFFSET_S [--fps 30]
A = VR (left eye), B = desktop. Prints delta = picture zero of B minus A, vs the released offset."""
import json, sys, pathlib, importlib.util
here = pathlib.Path(__file__).parent
s = importlib.util.spec_from_file_location("t", here / "x0_timing.py"); t = importlib.util.module_from_spec(s); s.loader.exec_module(t)
runs = pathlib.Path(sys.argv[1]); label = sys.argv[2]; off = float(sys.argv[3])
fps = int(sys.argv[sys.argv.index("--fps") + 1]) if "--fps" in sys.argv else 30
def go(role, eye):
    rows = json.load(open(runs / role / f"{role}-decoded.json"))
    return t.pic_zero(rows, fps, eye)
a = go(label + "A", "L"); ar = go(label + "A", "R"); b = go(label + "B", "D")
un = {}
for r in "AB":
    for line in open(runs / (label + r) / f"{label + r}-caps.jsonl"):
        j = json.loads(line)
        if j["kind"] == "unpause": un[r] = j
res = {"label": label, "released_offset_s": off,
       "delta_pixels_s": b["t0_mean"] - a["t0_mean"], "delta_pixels_R_eye_s": b["t0_mean"] - ar["t0_mean"],
       "sem_s": (a["t0_sem"] ** 2 + b["t0_sem"] ** 2) ** 0.5,
       "unpause_call_delta_s": un["B"]["t_unix"] - un["A"]["t_unix"],
       "unpause_late_by_s": {r: un[r]["t_unix"] - un[r]["target"] for r in "AB"},
       "n": [a["n"], b["n"]]}
try:
    aa = t.aud_zero(runs / (label + "A")); bb = t.aud_zero(runs / (label + "B"))
    res["delta_audio_s"] = bb["zero5"] - aa["zero5"]
    res["audio_minus_picture_A_ms"] = (aa["zero5"] - a["t0_mean"]) * 1e3
    res["audio_minus_picture_B_ms"] = (bb["zero5"] - b["t0_mean"]) * 1e3
except Exception as e:
    res["audio_error"] = repr(e)
print(json.dumps(res, indent=1))
