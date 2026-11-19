#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["numpy", "opencv-python-headless"]
# ///
"""Contact sheet: one capture each of left eye, right eye (native crops), the 64x64 xrprobe thumbnail
(upscaled, decoded or not) and the desktop window, annotated with the decoded frame number.
usage: x0_sheet.py OUT.png VR_RUNDIR VR_ROLE THUMB_RUNDIR THUMB_ROLE DESKTOP_RUNDIR DESKTOP_ROLE"""
import json, sys, pathlib, importlib.util
import numpy as np, cv2
here = pathlib.Path(__file__).parent
s = importlib.util.spec_from_file_location("bd", here / "blockclock_decode.py"); bd = importlib.util.module_from_spec(s); s.loader.exec_module(bd)
out, vr, vrole, tr, trole, dr, drole = sys.argv[1:8]
def pick(run, role, thumb=False, idx=40):
    f = "-thumb-decoded.json" if thumb else "-decoded.json"
    rows = json.load(open(pathlib.Path(run) / (role + f)))
    return rows
def img(run, f):
    p = pathlib.Path(run) / f
    if not p.exists(): p = pathlib.Path(run) / "xrprobe" / f
    return cv2.imread(str(p))
def label(im, text, ok):
    im = im.copy()
    cv2.rectangle(im, (0, 0), (im.shape[1], 34), (0, 0, 0), -1)
    cv2.putText(im, text, (8, 24), cv2.FONT_HERSHEY_SIMPLEX, 0.7, (0, 255, 0) if ok else (0, 0, 255), 2)
    return im
tiles = []
rows = pick(vr, vrole); mid = [r for r in rows if r["kind"] == "sparse"][8]["i"]
for eye in "LR":
    r = [x for x in rows if x["i"] == mid and x["eye"] == eye][0]
    im = img(vr, r["file"]); d = bd.decode_image(cv2.cvtColor(im, cv2.COLOR_BGR2RGB))
    tiles.append(label(cv2.resize(im, None, fx=640 / im.shape[1], fy=640 / im.shape[1], interpolation=cv2.INTER_NEAREST),
                       f"{eye} eye crop {im.shape[1]}x{im.shape[0]}: frame {d['frame']}" if d["ok"] else f"{eye} eye crop: unreadable", d["ok"]))
rows = pick(tr, trole, thumb=True)
for eye in "LR":
    r = [x for x in rows if x["eye"] == eye][3]
    im = img(tr, r["file"]); d = bd.decode_image(cv2.cvtColor(im, cv2.COLOR_BGR2RGB))
    big = cv2.resize(im, (640, 640), interpolation=cv2.INTER_NEAREST)
    tiles.append(label(big, f"{eye} 64x64 thumb of 320x240 eye buffer: " + (f"frame {d['frame']}" if d["ok"] else "unreadable"), d["ok"]))
rows = pick(dr, drole); r = [x for x in rows if x["kind"] == "sparse"][8]
im = img(dr, r["file"]); d = bd.decode_image(cv2.cvtColor(im, cv2.COLOR_BGR2RGB))
tiles.append(label(cv2.resize(im, (640, 360)), f"desktop window 1280x720: frame {d['frame']}" if d["ok"] else "desktop: unreadable", d["ok"]))
H = max(t.shape[0] for t in tiles)
def pad(t): return cv2.copyMakeBorder(t, 0, 640 - t.shape[0], 0, 0, cv2.BORDER_CONSTANT, value=(30, 30, 30))
rowA = np.hstack([pad(t) for t in tiles[:3]]); rowB = np.hstack([pad(t) for t in tiles[3:]] + [np.full((640, 640, 3), 30, np.uint8)])
cv2.imwrite(out, np.vstack([rowA, rowB]))
print(out)
