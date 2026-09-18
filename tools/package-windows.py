#!/usr/bin/env python3
"""Collect the cross-built Windows runtime and export the private project."""
import argparse, json, re, shutil, subprocess
from pathlib import Path
root = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser()
p.add_argument('--build-directory', type=Path, default=root/'.local/windows')
p.add_argument('--godot', required=True, help='Host editor used for PCK export')
p.add_argument('--out', type=Path, default=root/'dist/prim-windows')
p.add_argument('--objdump', default='x86_64-w64-mingw32-objdump')
p.add_argument('--strip', default='x86_64-w64-mingw32-strip')
p.add_argument('--runtime-directory', type=Path, action='append', default=[])
a = p.parse_args(); build=a.build_directory.resolve(); out=a.out.resolve(); out.mkdir(parents=True,exist_ok=True)
seeds = {
    root/'project/bin/windows/onnxruntime.dll': 'onnxruntime.dll',
    build/'godot/bin/godot.windows.template_debug.x86_64.exe': 'prim.exe',
    build/'rust-build/x86_64-pc-windows-gnu/debug/prim_native.dll': 'prim_native.dll',
    build/'video-build/bin/libmpv_zero.windows.template_debug.x86_64.dll': 'libmpv_zero.windows.template_debug.x86_64.dll',
    build/'steam-build/libgodot-steam-audio.windows.template_debug.x86_64.dll': 'libgodot-steam-audio.windows.template_debug.x86_64.dll',
    build/'msys/mingw64/bin/libmpv-2.dll': 'libmpv-2.dll',
}
search = [build/'msys/mingw64/bin', build/'sdk/lib/windows-x64'] + a.runtime_directory
index = {p.name.lower():p for folder in search for p in folder.glob('*.dll')}
for source, name in seeds.items(): shutil.copy2(source,out/name)
for source in (build/'sdk/lib/windows-x64').glob('*.dll'): shutil.copy2(source,out/source.name)
index.update({p.name.lower():p for p in out.glob('*.dll')})
queue = list(out.glob('*.dll'))+[out/'prim.exe']; seen=set(); external=set()
while queue:
    source=queue.pop()
    if source.name.lower() in seen: continue
    seen.add(source.name.lower())
    if source.parent != out: shutil.copy2(source,out/source.name)
    raw=subprocess.check_output([a.objdump,'-p',str(source)],text=True)
    for name in re.findall(r'DLL Name: (\S+)',raw):
        dependency=index.get(name.lower())
        if dependency: queue.append(dependency)
        else: external.add(name.lower())
missing_compiler_runtime = sorted(name for name in external if name.startswith(('libgcc', 'libstdc++', 'libwinpthread', 'libmcfgthread')))
if missing_compiler_runtime:
    raise RuntimeError('Missing compiler runtime DLLs; provide --runtime-directory: ' + ', '.join(missing_compiler_runtime))
# Keep full debug information in build outputs; ship the smaller runtime DLLs.
for name in seeds.values():
    subprocess.run([a.strip,'--strip-debug',str(out/name)],check=True)
(out/'bin/windows').mkdir(parents=True,exist_ok=True)
shutil.copy2(out/'libmpv-2.dll',out/'bin/windows/libmpv-2.dll')
for source,name in [('yt-dlp.exe','yt-dlp.exe'),('windows/deno.exe','deno.exe'),('VC_redist.x64.exe','VC_redist.x64.exe')]:
    shutil.copy2(root/'.local/media-tools'/source,out/name)
for launcher,flags in [('Play.bat',''),('Desktop.bat','-- --desktop'),('VR.bat','')]:
    (out/launcher).write_text('@echo off\ncd /d "%~dp0"\nset "PATH=%~dp0;%PATH%"\nprim.exe '+flags+'\n',newline='\r\n')
subprocess.run([a.godot,'--headless','--xr-mode','off','--path',str(root/'project'),'--export-pack','Windows',str(out/'prim.pck')],check=True)
(out/'system-runtime-imports.json').write_text(json.dumps(sorted(external),indent=2)+'\n')
print('Windows bundle:',out)

# Keep bundled avatar and vendored implementation notices readable outside the PCK.
notices = out / 'licenses' / 'avatars'
notices.mkdir(parents=True, exist_ok=True)
for name in ['ATTRIBUTION.md', 'LICENSE_SAMPLES.txt']:
    shutil.copy2(root / 'project/avatars/models' / name, notices / name)
for addon in ['vrm', 'Godot-MToon-Shader', 'renik']:
    for source in (root / 'project/addons' / addon).glob('LICENSE*'):
        shutil.copy2(source, notices / (addon + '-' + source.name))

viseme_notices = out / 'licenses' / 'visemes'
viseme_notices.mkdir(parents=True, exist_ok=True)
for source in (root / 'native/viseme-model').iterdir():
    if source.name in ['LICENSE', 'NOTICE.md', 'THIRD_PARTY_NOTICES.md', 'config.json']:
        shutil.copy2(source, viseme_notices / source.name)
for source in (root / '.local/viseme-runtime/windows').iterdir():
    if source.name in ['LICENSE', 'ThirdPartyNotices.txt']:
        shutil.copy2(source, viseme_notices / ('onnxruntime-' + source.name))
