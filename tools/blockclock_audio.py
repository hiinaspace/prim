#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["numpy"]
# ///
"""Record a PulseAudio/PipeWire *monitor of a private null sink* with wall-clock stamps, never real speakers.

  blockclock_audio.py record SINK OUT_PREFIX [--seconds N]   # until SIGTERM/SIGINT or N seconds
        writes OUT_PREFIX.wav (mono mix, 48 kHz) and OUT_PREFIX.json (chunk timeline)
  blockclock_audio.py ticks OUT_PREFIX                       # tick onsets as unix times + error bar

Timing model: chunk i is read at unix time t_i after n_i samples exist, so sample s was at the
sink no later than t_i - (n_i - s)/rate for every chunk after it. Stream zero z = min_i(t_i - n_i/rate)
(an upper bound on delivery lag of one chunk, ~chunk size). err = p25 - min of that quantity.
"""
import json, signal, subprocess, sys, time, wave
import numpy as np
import importlib.util, pathlib

RATE = 48000


def record(sink, prefix, seconds=None):
    assert sink.startswith("x0") or "null" in sink, "refusing to record a non-test sink"
    p = subprocess.Popen(["parec", "-d", sink + ".monitor", "--raw", "--format=s16le", "--rate=%d" % RATE,
                          "--channels=2", "--latency-msec=10"], stdout=subprocess.PIPE, bufsize=0)
    stop = [False]
    signal.signal(signal.SIGTERM, lambda *a: stop.__setitem__(0, True))
    signal.signal(signal.SIGINT, lambda *a: stop.__setitem__(0, True))
    chunks, buf, n = [], bytearray(), 0
    t0 = time.time()
    while not stop[0] and (seconds is None or time.time() - t0 < seconds):
        b = p.stdout.read(1920)  # 480 frames of s16 stereo = 10 ms
        if not b:
            break
        t = time.time()
        buf += b
        n += len(b) // 4
        chunks.append((t, n))
    p.terminate()
    pcm = np.frombuffer(bytes(buf[:len(buf) // 4 * 4]), "<i2").reshape(-1, 2)
    with wave.open(prefix + ".wav", "wb") as w:
        w.setnchannels(2); w.setsampwidth(2); w.setframerate(RATE); w.writeframes(pcm.tobytes())
    json.dump({"rate": RATE, "chunks": chunks}, open(prefix + ".json", "w"))


def ticks(prefix):
    here = pathlib.Path(__file__).parent
    spec = importlib.util.spec_from_file_location("bd", here / "blockclock_decode.py")
    bd = importlib.util.module_from_spec(spec); spec.loader.exec_module(bd)
    with wave.open(prefix + ".wav") as w:
        d = np.frombuffer(w.readframes(w.getnframes()), "<i2").astype(np.float32) / 32768
    mono = d.reshape(-1, 2).mean(1)
    tl = json.load(open(prefix + ".json"))
    ch = np.array(tl["chunks"])
    lag = ch[:, 0] - ch[:, 1] / RATE
    z, err = float(lag.min()), float(np.percentile(lag, 25) - lag.min())
    out = []
    for t in bd.find_ticks(mono, RATE):
        out.append({**t, "unix": z + t["t"]})
    return {"zero_unix": z, "err_s": err, "ticks": out}


if __name__ == "__main__":
    if sys.argv[1] == "record":
        sec = float(sys.argv[sys.argv.index("--seconds") + 1]) if "--seconds" in sys.argv else None
        record(sys.argv[2], sys.argv[3], sec)
    else:
        print(json.dumps(ticks(sys.argv[2])))
