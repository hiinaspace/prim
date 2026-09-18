#!/usr/bin/env python3
"""Measure the real mpv -> Steam Audio -> Godot output with deterministic noise.
Run: nix develop --command uv run --with numpy python tools/test-spatial-audio.py
Uses a private output sink/profile; no headset or normal audio device is changed.
"""
import json
import os
from pathlib import Path
import subprocess
import time
import numpy as np

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / '.local/spatial-audio' / time.strftime('%Y%m%d-%H%M%S')
OUT.mkdir(parents=True)
subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-y', '-f', 'lavfi', '-i', 'color=c=black:s=320x180:r=10', '-f', 'lavfi', '-i', 'anoisesrc=color=white:amplitude=0.1:sample_rate=48000:seed=42', '-filter_complex', '[1:a]pan=stereo|c0=c0|c1=c0[a]', '-map', '0:v', '-map', '[a]', '-t', '60', '-c:v', 'libx264', '-preset', 'ultrafast', '-c:a', 'pcm_f32le', str(OUT/'noise.mkv')], check=True)
sink = 'prim_spectrum_' + str(os.getpid())
module = subprocess.check_output(['pactl', 'load-module', 'module-null-sink', 'sink_name='+sink, 'rate=48000', 'channels=2', 'norewinds=1'], text=True).strip()
try:
    env = os.environ | {'DISPLAY':os.environ.get('DISPLAY', ':0'), 'PULSE_SINK':sink, 'XDG_DATA_HOME':str(OUT/'profile'), 'PRIM_AUDIO_PROBE':str(OUT), 'LIBMPV_ZERO_MPV_LIBRARY':str(ROOT/'.local/mpv/lib/libmpv.so.2')}
    with (OUT/'run.log').open('w') as log:
        subprocess.run([str(ROOT/'.local/godot/bin/godot'), '--path', str(ROOT/'project'), '--display-driver', 'x11', '--xr-mode', 'off', '--max-fps', '60', '--script', 'res://tests/spatial_audio.gd', '--', '--desktop'], env=env, stdout=log, stderr=subprocess.STDOUT, timeout=75, check=True)
finally:
    subprocess.run(['pactl', 'unload-module', module], check=True)
log = (OUT/'run.log').read_text()
assert 'ERROR' not in log and 'leaked' not in log, log
rate = json.loads((OUT/'rate.json').read_text())['rate']
bands = [(200,1000), (1000,3000), (3000,6000), (6000,10000), (10000,16000)]
results = {}
for p in OUT.glob('*.f32'):
    samples = np.fromfile(p, dtype='<f4').reshape(-1,2)
    assert len(samples) > rate and np.isfinite(samples).all(), p
    n = 8192
    blocks = samples[:len(samples)//n*n].reshape(-1,n,2)
    power = (abs(np.fft.rfft(blocks*np.hanning(n)[None,:,None],axis=1))**2).mean(axis=0)
    freq = np.fft.rfftfreq(n, 1/rate)
    db = [float(10*np.log10(power[(freq>=lo)&(freq<hi)].mean()+1e-30)) for lo,hi in bands]
    results[p.stem] = {'bands_db':db, 'relative_low_db':[v-db[0] for v in db], 'ear_rms':np.sqrt((samples**2).mean(axis=0)).tolist()}
# Same mono source, one speaker: isolate processing from interspeaker interference.
for band in (1,2):
    assert results['point_single']['relative_low_db'][band] - results['legacy_single']['relative_low_db'][band] > 4, 'Point-source path regressed to low-order coloration'
    assert abs(results['bypass_single']['relative_low_db'][band] - results['direct']['relative_low_db'][band]) < 1.5, 'Unexpected non-HRTF filtering'
for mode, louder in [('point_right',1), ('point_left',0), ('point_turned',1)]:
    ears = results[mode]['ear_rms']
    assert ears[louder] > ears[1-louder]*1.4, (mode,ears)
alignment = {}
for mode in ('bypass_pair', 'bypass_pair_again'):
    left, right = [np.fromfile(OUT/f'{mode}-channel{i}.raw', dtype='<f4') for i in range(2)]
    count = min(len(left),len(right))
    a, b = left[:count], right[:count]
    error = float(np.mean((a-b)**2) / max(np.mean(a*a+b*b),1e-20))
    alignment[mode] = error
    assert error < 1e-8, (mode, 'Split-channel timing mismatch', error)
(OUT/'alignment.json').write_text(json.dumps(alignment)+'\n')
print('SPLIT_CHANNEL_ALIGNMENT', alignment)
(OUT/'results.json').write_text(json.dumps({'sample_rate':rate, 'bands_hz':bands, 'measurements':results},indent=2)+'\n')
print('SPATIAL_AUDIO_RESULT PASS', OUT)
for name, result in results.items(): print(name, [round(v,2) for v in result['relative_low_db']], result['ear_rms'])
