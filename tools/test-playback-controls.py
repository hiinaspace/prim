#!/usr/bin/env python3
"""Generated stereo/subtitle fixtures; isolated output sink and preferences."""
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / '.local' / 'playback-controls'
OUT.mkdir(parents=True, exist_ok=True)
for name, caption in [('en', 'FIRST SUBTITLE TRACK'), ('ja', 'SECOND SUBTITLE TRACK')]:
    (OUT / (name + '.srt')).write_text('1\n00:00:00,000 --> 00:01:00,000\n' + caption + '\n')
for name, audio in [('stereo', '0.15*sin(2*PI*440*t)|0.15*sin(2*PI*880*t)'), ('same', '0.15*sin(2*PI*440*t)|0.15*sin(2*PI*440*t)'), ('surround', '0|0|0.15*sin(2*PI*440*t)|0|0|0:c=5.1')]:
    command = ['ffmpeg', '-hide_banner', '-loglevel', 'error', '-y', '-f', 'lavfi', '-i', 'color=c=black:s=640x360:r=30', '-f', 'lavfi', '-i', f'aevalsrc={audio}:s=48000:d=60']
    if name == 'stereo':
        command += ['-i', str(OUT/'en.srt'), '-i', str(OUT/'ja.srt'), '-map', '0:v', '-map', '1:a', '-map', '2', '-map', '3', '-c:s', 'srt', '-metadata:s:s:0', 'language=eng', '-metadata:s:s:1', 'language=jpn', '-metadata:s:s:0', 'title=English test', '-metadata:s:s:1', 'title=Japanese test']
    command += ['-t', '60', '-c:v', 'libx264', '-preset', 'ultrafast', '-c:a', 'pcm_f32le', str(OUT/(name+'.mkv'))]
    subprocess.run(command, check=True)
sink = 'prim_controls_' + str(os.getpid())
module = subprocess.check_output(['pactl', 'load-module', 'module-null-sink', 'sink_name='+sink, 'rate=48000', 'channels=2', 'norewinds=1'], text=True).strip()
try:
    env = os.environ | {'PULSE_SINK': sink, 'XDG_DATA_HOME': str(OUT/'profile'), 'PRIM_TEST_CONTROLS_DIR': str(OUT)}
    command = [os.environ.get('GODOT', str(ROOT/'.local/godot/bin/godot')), '--path', str(ROOT/'project'), '--display-driver', 'x11', '--xr-mode', 'off', '--max-fps', '60', '--script', 'res://tests/playback_controls.gd', '--', '--desktop']
    if os.environ.get('PRIM_TEST_WINDOWS_EXE'):
        command = ['wine', os.environ['PRIM_TEST_WINDOWS_EXE'], '--xr-mode', 'off', '--max-fps', '60', '--', '--desktop']
        env['PRIM_TEST_CONTROLS_DIR'] = 'Z:' + str(OUT).replace('/', '\\')
        env.pop('LIBMPV_ZERO_MPV_LIBRARY', None)
        env['WINEDLLOVERRIDES'] = 'winemenubuilder.exe,mscoree,mshtml=;vulkan-1=b'
        env['WINEDEBUG'] = '-all'
    with (OUT/'run.log').open('w') as log:
        result = subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT, timeout=120)
    print((OUT/'run.log').read_text())
    raise SystemExit(result.returncode)
finally:
    subprocess.run(['pactl', 'unload-module', module], check=True)
