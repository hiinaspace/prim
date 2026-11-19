#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["numpy", "opencv-python-headless"]
# ///
"""Decode every capture of an X0 run and relate it to the audio tick stream.
usage: x0_analyze.py RUNDIR ROLE [--fps 30]  -> prints JSON summary; writes RUNDIR/ROLE-decoded.json"""
import json, sys, pathlib, importlib.util
import numpy as np, cv2
here = pathlib.Path(__file__).parent
def load(n):
    s = importlib.util.spec_from_file_location(n, here / (n + ".py")); m = importlib.util.module_from_spec(s); s.loader.exec_module(m); return m
bd = load("blockclock_decode")

def main():
    run = pathlib.Path(sys.argv[1]); role = sys.argv[2]
    fps = int(sys.argv[sys.argv.index("--fps") + 1]) if "--fps" in sys.argv else 30
    rows = []
    for line in open(run / f"{role}-caps.jsonl"):
        r = json.loads(line)
        if "files" not in r or r.get("kind") == "thumb" and "--thumb" not in sys.argv or r.get("kind") != "thumb" and "--thumb" in sys.argv: continue
        for k, f in enumerate(r["files"]):
            p = run / f if (run / f).exists() else run / "xrprobe" / f
            im = cv2.imread(str(p), cv2.IMREAD_COLOR)
            d = bd.decode_image(cv2.cvtColor(im, cv2.COLOR_BGR2RGB))
            d.pop("quad", None)
            eye = ["L", "R"][k] if len(r["files"]) == 2 else "D"
            rows.append({"i": r.get("i", -1), "kind": r["kind"], "eye": eye, "t_unix": r["t_unix"], "frame": d["frame"], "ok": d["ok"],
                         "why": d["why"], "margin": d.get("margin"), "file": f, "wh": [im.shape[1], im.shape[0]]})
    json.dump(rows, open(run / (f"{role}-thumb-decoded.json" if "--thumb" in sys.argv else f"{role}-decoded.json"), "w"))
    out = {}
    for eye in sorted({r["eye"] for r in rows}):
        rs = [r for r in rows if r["eye"] == eye]
        ok = [r for r in rs if r["ok"]]
        out[eye] = {"n": len(rs), "decoded": len(ok), "rate": len(ok) / max(1, len(rs)),
                    "whys": sorted({r["why"] for r in rs if not r["ok"]}), "min_margin": min([r["margin"] for r in ok], default=None)}
    print(json.dumps(out))
main()
