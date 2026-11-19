#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["numpy"]
# ///
"""Block-clock test video for Prim's sync oracle (round X0).

Every frame shows its own frame number as a Gray-coded grid of large black/white
cells inside a white border, so a decoder can read it back from a perspective-
distorted, downscaled capture of the theater screen. The audio carries a short
tick at the start of every second (1 kHz, 20 ms) and a distinct tick every 10 s
(2 kHz, 60 ms), onset exactly at frame k*fps.

Layout (1280x720 default), see LAYOUT below:
  - 40 px white border; a filled 90 px square in the top-left inner corner (orientation)
  - grid COLS x ROWS cells, row 0 is a calibration row alternating W,B,W,B,...
    (threshold + sanity), rows 1.. carry 20 Gray-code bits (MSB first, row-major)
    followed by a 4-bit check (xor of the five 4-bit nibbles, then ^ 0xA)
Usage: blockclock.py OUT.mp4 [--fps 30|60] [--seconds 120] [--cell 160]
"""
import argparse, subprocess, sys
import numpy as np

W, H = 1280, 720
BORDER = 40
COLS, ROWS = 6, 5  # row 0 calibration, 4 data rows of 6 = 24 cells = 20 bits + 4 check
NBITS = 20


def gray(n: int) -> int:
    return n ^ (n >> 1)


def ungray(g: int) -> int:
    n = 0
    while g:
        n ^= g
        g >>= 1
    return n


def check4(g: int) -> int:
    x = 0
    for i in range(5):
        x ^= (g >> (4 * i)) & 0xF
    return x ^ 0xA


def cell_bits(frame: int) -> list[int]:
    g = gray(frame % (1 << NBITS))
    bits = [(g >> (NBITS - 1 - i)) & 1 for i in range(NBITS)]
    c = check4(g)
    bits += [(c >> (3 - i)) & 1 for i in range(4)]
    return bits


def geometry(cell: int):
    gw, gh = COLS * cell, ROWS * (cell * 7 // 10)
    ch = cell * 7 // 10
    x0 = (W - gw) // 2
    y0 = (H - gh) // 2
    return x0, y0, cell, ch


def render(frame: int, cell: int) -> np.ndarray:
    img = np.zeros((H, W), np.uint8)
    img[:BORDER, :] = 255; img[-BORDER:, :] = 255; img[:, :BORDER] = 255; img[:, -BORDER:] = 255
    img[BORDER:BORDER + 90, BORDER:BORDER + 90] = 255  # orientation: only the top-left corner has a filled square
    x0, y0, cw, ch = geometry(cell)
    # calibration row
    for c in range(COLS):
        if c % 2 == 0:
            img[y0:y0 + ch, x0 + c * cw:x0 + (c + 1) * cw] = 255
    bits = cell_bits(frame)
    for i, b in enumerate(bits):
        r, c = 1 + i // COLS, i % COLS
        if b:
            img[y0 + r * ch:y0 + (r + 1) * ch, x0 + c * cw:x0 + (c + 1) * cw] = 255
    # thin dark gutters between cells would hurt downscaled reads; none are used.
    # Cells of equal value merge by design; the calibration row gives the grid pitch.
    return img


def audio(seconds: int, sr=48000) -> np.ndarray:
    a = np.zeros(seconds * sr, np.float32)
    for s in range(seconds):
        big = s % 10 == 0
        f, d = (2000.0, 0.060) if big else (1000.0, 0.020)
        n = int(d * sr)
        t = np.arange(n) / sr
        env = np.sin(np.pi * np.arange(n) / n) ** 0.5  # soft edges, sharp-ish onset
        a[s * sr:s * sr + n] = 0.8 * np.sin(2 * np.pi * f * t) * env
    return a


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("--fps", type=int, default=30, choices=[30, 60])
    ap.add_argument("--seconds", type=int, default=120)
    ap.add_argument("--cell", type=int, default=160)
    a = ap.parse_args()
    sr = 48000
    wav = a.out + ".tmp.wav"
    pcm = (audio(a.seconds, sr) * 32767).astype("<i2")
    import wave
    with wave.open(wav, "wb") as w:
        w.setnchannels(1); w.setsampwidth(2); w.setframerate(sr); w.writeframes(pcm.tobytes())
    cmd = ["ffmpeg", "-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "gray", "-s", f"{W}x{H}",
           "-r", str(a.fps), "-i", "-", "-i", wav, "-c:v", "libx264", "-preset", "veryfast", "-crf", "14",
           "-pix_fmt", "yuv420p", "-g", str(a.fps), "-c:a", "aac", "-b:a", "128k", "-ac", "2",
           "-shortest", a.out]
    p = subprocess.Popen(cmd, stdin=subprocess.PIPE)
    for f in range(a.seconds * a.fps):
        p.stdin.write(render(f, a.cell).tobytes())
    p.stdin.close(); p.wait()
    import os; os.remove(wav)


if __name__ == "__main__":
    main()
