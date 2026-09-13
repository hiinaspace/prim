#!/usr/bin/env python3
"""Compare the Rust frontend/inference against an independent NumPy/ORT reference.
Requires numpy and onnxruntime, plus a built target/debug/examples/viseme_probe.
Input is mono f32le; output artifacts stay outside exported project resources.
"""
import argparse, json, subprocess
from pathlib import Path
import numpy as np
import onnxruntime as ort
ROOT = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser(description=__doc__)
p.add_argument('pcm', type=Path)
p.add_argument('--rate', type=int, required=True)
p.add_argument('--runtime', type=Path, required=True)
p.add_argument('--out', type=Path, required=True)
a = p.parse_args(); a.out.mkdir(parents=True, exist_ok=True)
subprocess.run([str(ROOT/'target/debug/examples/viseme_probe'), str(a.runtime), str(a.pcm), str(a.rate), '1', str(a.out/'rust.csv'), str(a.out/'mel.f32')], check=True)
x = np.fromfile(a.pcm, dtype='<f4')
if a.rate != 16000:
    # The mathematical FIR contract, evaluated independently in vectorized form.
    n = np.arange(48, dtype=np.float64)
    phases = np.arange(1024, dtype=np.float64)[:, None] / 1024
    fc = .5 * min(1, 16000 / a.rate) * .9
    kernel = 2 * fc * np.sinc(2 * fc * (n - 23 - phases)) * (.42 - .5*np.cos(2*np.pi*n/47) + .08*np.cos(4*np.pi*n/47))
    coeff = kernel.astype(np.float32) * (1 / kernel.sum(axis=1))[:, None].astype(np.float32)
    padded = np.r_[np.zeros(24, dtype=np.float32), x]
    clock = np.arange(int(len(x)*16000/a.rate)+1, dtype=np.int64) * a.rate + 23*16000
    center = clock // 16000
    valid = center + 24 < len(padded)
    center, clock = center[valid], clock[valid]
    phase = np.rint(clock % 16000 * (1024/16000)).astype(int) % 1024
    windows = np.lib.stride_tricks.sliding_window_view(padded, 48)[center-23]
    x = (windows * coeff[phase]).sum(axis=1, dtype=np.float32)
# Uncentered, hop-aligned history with the same 240-sample initial zero history.
count = len(x)//160
windows = np.lib.stride_tricks.sliding_window_view(np.r_[np.zeros(240,dtype=np.float32),x],400)[::160][:count]
window = np.hanning(400).astype(np.float32)
power = abs(np.fft.rfft(windows*window, n=1024))**2
mel_lo = 2595*np.log10(1+50/700); mel_hi = 2595*np.log10(1+8000/700)
edges = 1025*700*(10**(np.linspace(mel_lo,mel_hi,82)/2595)-1)/16000
bins = np.arange(513)
filters = np.array([np.maximum(0,np.minimum((bins-l)/(c-l),(r-bins)/(r-c))) for l,c,r in zip(edges,edges[1:],edges[2:])])
mel = (10*np.log10(np.maximum(power@filters.T,1e-10))).astype(np.float32)
rust = np.fromfile(a.out/'mel.f32',dtype='<f4').reshape(-1,80)
assert rust.shape == mel.shape, (rust.shape,mel.shape)
# Compare energetic bins in dB; near the numerical floor compare absolute power.
active = mel > -65
mel_error = float(np.max(abs(mel[active]-rust[active])))
power_error = float(np.max(abs(10**(mel/10)-10**(rust/10))))
assert mel_error < .03, mel_error
options = ort.SessionOptions(); options.intra_op_num_threads=1; options.inter_op_num_threads=1
session = ort.InferenceSession(str(ROOT/'native/viseme-model/model.onnx'),options,providers=['CPUExecutionProvider'])
caches = {v.name:np.zeros(v.shape,dtype=np.float32) for v in session.get_inputs() if v.name.startswith('cache_')}
weights=[]
for frame in mel:
    output=dict(zip([o.name for o in session.get_outputs()],session.run(None,dict(caches,audio_features=frame.reshape(1,1,80)))))
    weights.append((1/(1+np.exp(-np.clip(output['viseme_logits'],-50,50)))).reshape(15))
    caches={f'cache_{i}':output[f'cache_out_{i}'] for i in range(10)}
rust_weights=np.loadtxt(a.out/'rust.csv',delimiter=',',skiprows=1)[:,1:]
weight_error=float(np.max(abs(np.array(weights)-rust_weights)))
assert weight_error < .005, weight_error
report={'rate':a.rate,'frames':len(mel),'max_active_mel_error_db':mel_error,'max_power_error':power_error,'max_weight_error':weight_error,'python_ort':ort.__version__}
(a.out/'report.json').write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report))
