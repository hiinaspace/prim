#!/usr/bin/env python3
"""Two local Godot processes, synthetic microphone, isolated PulseAudio sinks."""
import functools
import http.server
import json
import os
from pathlib import Path
import subprocess
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / '.local' / 'integration'
OUT.mkdir(parents=True, exist_ok=True)
def run(*args):
    return subprocess.check_output(args, text=True).strip()
def wait_file(path, process, timeout=20):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if path.exists() and path.stat().st_size: return
        if process.poll() is not None: raise RuntimeError(f'Process exited before {path.name}')
        time.sleep(.1)
    raise TimeoutError(path)
if not (OUT / 'media.mp4').exists():
    subprocess.run(['ffmpeg','-hide_banner','-loglevel','error','-y','-f','lavfi','-i','testsrc2=size=640x360:rate=30','-f','lavfi','-i','sine=frequency=330:sample_rate=48000','-t','60','-c:v','libx264','-preset','ultrafast','-pix_fmt','yuv420p','-c:a','aac',str(OUT/'media.mp4')], check=True)
if not (OUT/'mic.wav').exists():
    subprocess.run(['ffmpeg','-hide_banner','-loglevel','error','-y','-f','lavfi','-i','sine=frequency=620:sample_rate=48000','-t','65','-af','volume=3','-ac','2',str(OUT/'mic.wav')], check=True)
host_file = OUT / 'media.mp4'
if os.environ.get('PRIM_TEST_CONTAINER') == 'mkv':
    captions = OUT / 'captions.srt'
    captions.write_text('1\n00:00:00,000 --> 00:00:30,000\nPrim shared-file subtitle fixture\n')
    host_file = OUT / 'media.mkv'
    subprocess.run(['ffmpeg', '-hide_banner', '-loglevel', 'error', '-y', '-i', str(OUT/'media.mp4'), '-i', str(captions), '-map', '0', '-map', '1', '-c', 'copy', str(host_file)], check=True)
class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_): pass
    def send_head(self):
        path = Path(self.translate_path(self.path))
        if not path.is_file(): return super().send_head()
        size = path.stat().st_size
        start, end = 0, size - 1
        requested = self.headers.get('Range')
        if requested:
            bounds = requested.removeprefix('bytes=').split('-')
            start = int(bounds[0] or 0)
            end = min(int(bounds[1]) if bounds[1] else end, end)
            if start > end:
                self.send_error(416); return None
        self.send_response(206 if requested else 200)
        self.send_header('Content-Type', self.guess_type(str(path)))
        self.send_header('Accept-Ranges', 'bytes')
        self.send_header('Content-Length', str(end-start+1))
        if requested: self.send_header('Content-Range', f'bytes {start}-{end}/{size}')
        self.end_headers()
        stream = path.open('rb')
        stream.seek(start)
        self.remaining = end-start+1
        return stream
    def copyfile(self, source, target):
        try:
            while self.remaining:
                block = source.read(min(self.remaining, 65536))
                if not block: break
                target.write(block)
                self.remaining -= len(block)
        except (BrokenPipeError, ConnectionResetError): pass
server = http.server.ThreadingHTTPServer(('127.0.0.1',0), functools.partial(Quiet,directory=str(OUT)))
threading.Thread(target=server.serve_forever,daemon=True).start()
modules=[]
processes=[]
logs=[]
try:
    suffix = str(os.getpid())
    mic = 'prim_test_mic_' + suffix
    speaker = 'prim_test_output_' + suffix
    for name in (mic,speaker):
        modules.append(run('pactl','load-module','module-null-sink',f'sink_name={name}','rate=48000','channels=2','norewinds=1'))
    tone = subprocess.Popen(['paplay','--device='+mic,str(OUT/'mic.wav')])
    processes.append(tone)
    base = os.environ.copy()
    base.update(DISPLAY=base.get('DISPLAY',':0'), PRIM_NETWORK_LOCAL_ONLY='1', PULSE_SOURCE=mic+'.monitor', PULSE_SINK=speaker, PRIM_TEST_MEDIA=f'http://127.0.0.1:{server.server_port}/media.mp4')
    base['PRIM_TEST_HOST_FILE'] = str(host_file)
    base.pop('PRIM_AUTOJOIN',None)
    base.pop('PRIM_MEDIA',None)
    godot = os.environ.get('GODOT','godot')
    command = [godot,'--path',str(ROOT/'project'),'--display-driver','x11','--xr-mode','off','--disable-vsync','--max-fps','60','--script','res://tests/'+os.environ.get('PRIM_TEST_SCRIPT','integration.gd'),'--','--desktop']
    endpoint=OUT/'host.endpoint'
    endpoint.unlink(missing_ok=True)
    roles = ['host'] + ['client'+str(i) for i in range(int(os.environ.get('PRIM_TEST_PEERS','1')))]
    for role in roles:
        (OUT/(role+'.json')).unlink(missing_ok=True)
        env=base | {'PRIM_TEST_ROLE':role,'PRIM_TEST_OUTPUT':str(OUT/role),'XDG_DATA_HOME':str(OUT/('profile-'+role))}
        if role!='host': env['PRIM_TEST_HOST']=endpoint.read_text()
        log=open(OUT/(role+'.log'),'w')
        logs.append(log)
        launch = command
        windows = base.get('PRIM_TEST_WINDOWS_EXE')
        if role != 'host' and windows:
            launch = ['wine', windows, '--rendering-driver', 'vulkan', '--xr-mode', 'off', '--disable-vsync', '--max-fps', '60', '--', '--desktop']
            env['PRIM_TEST_OUTPUT'] = 'Z:' + str(OUT/role).replace('/', '\\')
            env['PRIM_TEST_HOST_FILE'] = 'Z:' + str(host_file).replace('/', '\\')
            env.pop('LIBMPV_ZERO_MPV_LIBRARY', None)
            env.pop('LIBMPV_ZERO_VULKAN_LIBRARY', None)
            env['WINEDLLOVERRIDES'] = 'winemenubuilder.exe,mscoree,mshtml=;vulkan-1=b'
            env['WINEDEBUG'] = '-all'
            # Wine may expose no interface addresses to the minimal endpoint.
            # Opt into normal discovery/relays while retaining the explicit test
            # host, so this does not discover or join a user's real room.
            env['PRIM_NETWORK_LOCAL_ONLY'] = base.get('PRIM_TEST_WINDOWS_LOCAL_ONLY', '1')
        process=subprocess.Popen(launch,env=env,stdout=log,stderr=subprocess.STDOUT)
        processes.append(process)
        if role=='host': wait_file(endpoint,process)
    for process in processes[1:]:
        process.wait(timeout=110)
    reports=[json.loads((OUT/(role+'.json')).read_text()) for role in roles]
    print(json.dumps(reports,indent=2))
    if any(report['failures'] for report in reports): raise SystemExit(1)
finally:
    for process in processes:
        if process.poll() is None:
            process.terminate()
            try: process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill(); process.wait()
    for log in logs: log.close()
    server.shutdown()
    for module in reversed(modules): subprocess.run(['pactl','unload-module',module],check=False)
