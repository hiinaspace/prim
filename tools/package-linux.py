#!/usr/bin/env python3
"""Bundle a staged Linux runtime; keep graphics drivers supplied by the host."""
import argparse, json, os, re, shutil, subprocess, sys
from pathlib import Path
root=Path(__file__).resolve().parents[1]
parser=argparse.ArgumentParser()
parser.add_argument('--godot',required=True)
parser.add_argument('--mpv',required=True)
parser.add_argument('--out',default=str(root/'dist/prim-linux'))
parser.add_argument('--glibc', help='Prebuilt matched glibc runtime; defaults to pinned Nix build')
args=parser.parse_args()
out=Path(args.out).resolve(); out.mkdir(parents=True,exist_ok=True)
lib=out/'lib'; lib.mkdir(exist_ok=True)
def call(*args):return subprocess.check_output(args,text=True).strip()
def copy(source,destination):
    destination.parent.mkdir(parents=True,exist_ok=True)
    shutil.copy2(source,destination); destination.chmod(0o755)
seeds={Path(args.godot).resolve():out/'prim.bin',Path(args.mpv).resolve():out/'bin/linux/libmpv.so.2'}
runtime = root / 'project/bin/linux/libonnxruntime.so'
if not runtime.is_file(): raise RuntimeError('Run tools/fetch-viseme-runtime.py --platform linux first')
for path in (root/'project/bin/linux').glob('*.so'):seeds[path]=out/'bin/linux'/path.name
for path in (root/'project/addons/godot-steam-audio/bin').glob('*.so'):
    if path.name != 'libphonon.so' and '.linux.template_debug.x86_64.so' not in path.name: continue
    seeds[path]=out/'addons/godot-steam-audio/bin'/path.name
# Godot loads several platform backends dynamically, so ldd alone misses them.
pattern=re.compile(r'lib(?:X[^/]*|wayland[^/]*|xkbcommon[^/]*|pulse[^/]*|asound[^/]*|openxr[^/]*|vulkan[^/]*|GL[^/]*|EGL[^/]*|udev[^/]*|dbus[^/]*|SDL[^/]*)\.so(?:\..*)?$')
for source in list(seeds):
    for directory in call('patchelf','--print-rpath',str(source)).split(':'):
        if not directory.startswith('/'):continue
        for path in Path(directory).glob('*.so*'):
            if pattern.match(path.name):seeds.setdefault(path,lib/path.name)
queue=list(seeds); dependencies={}; seen=set()
while queue:
    source=queue.pop()
    if source in seen:continue
    seen.add(source)
    text=call('ldd',str(source))
    missing = re.findall(r'(\S+) => not found', text)
    if any(name not in dependencies for name in missing):raise RuntimeError(text)
    for path in re.findall(r'(?:=>\s+|^\s*)(/[^\s]+)',text,re.M):
        dependency=Path(path)
        if not dependency.exists():continue
        previous=dependencies.get(dependency.name)
        if previous and previous.resolve()!=dependency.resolve():
            # A single SONAME must have a coherent provider throughout the bundle.
            if previous.read_bytes()!=dependency.read_bytes():
                print(f'Using first resolved SONAME provider for {dependency.name}: {previous}')
                continue
        if dependency.name not in dependencies:
            dependencies[dependency.name]=dependency;queue.append(dependency)
for name,source in dependencies.items():copy(source,lib/name)
for source,destination in seeds.items():copy(source,destination)
# Original build outputs retain debug information for crash investigation.
for path in (out/'bin/linux').glob('*.so'):
    subprocess.run(['strip','--strip-debug',str(path)],check=True)
for path in out.rglob('*'):
    if not path.is_file() or path.name.startswith('ld-linux'):continue
    with path.open('rb') as f:magic=f.read(4)
    if magic!=b'\x7fELF':continue
    # Some Nix libraries (notably mpv's mujs dependency) encode an absolute
    # DT_NEEDED path. RPATH rewriting alone cannot redirect those dependencies.
    for needed in call('patchelf','--print-needed',str(path)).splitlines():
        if needed.startswith('/nix/store/'):
            name=Path(needed).name
            if not (lib/name).is_file():raise RuntimeError(f'Missing bundled dependency: {needed}')
            subprocess.run(['patchelf','--replace-needed',needed,name,str(path)],check=True)
    relative=os.path.relpath(lib,path.parent)
    subprocess.run(['patchelf','--set-rpath','$ORIGIN' if relative=='.' else '$ORIGIN/'+relative,str(path)],check=True)
# The package is explicitly mounted: loading through the bundled glibc loader
# must not make Godot infer its resource directory from /proc/self/exe.
launcher='''#!/usr/bin/env bash
set -euo pipefail
app_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
export PATH="$app_dir/tools:$PATH"
export LIBMPV_ZERO_MPV_LIBRARY="$app_dir/bin/linux/libmpv.so.2"
export LIBMPV_ZERO_VULKAN_LIBRARY="$app_dir/lib/libvulkan.so.1"
# Driver manifests can name host libraries by SONAME. The bundled Nix glibc's
# default search paths do not cover a normal distro's multiarch library folders.
runtime_library_path="$app_dir/lib"
if [[ -n "${LD_LIBRARY_PATH:-}" ]]; then runtime_library_path+=":$LD_LIBRARY_PATH"; fi
for directory in /usr/lib/x86_64-linux-gnu /lib/x86_64-linux-gnu /usr/lib64 /usr/lib /lib64 /lib /run/opengl-driver/lib; do
    if [[ -d "$directory" ]]; then runtime_library_path+=":$directory"; fi
done
export XDG_DATA_DIRS="${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
exec "$app_dir/lib/ld-linux-x86-64.so.2" --argv0 "$app_dir/prim.bin" --library-path "$runtime_library_path" "$app_dir/prim.bin" --path "$app_dir" --main-pack "$app_dir/prim.pck" "$@"
'''
(out/'prim').write_text(launcher);(out/'prim').chmod(0o755)
(out/'desktop').write_text('''#!/usr/bin/env bash
set -euo pipefail
app_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
exec "$app_dir/prim" "$@" -- --desktop
''');(out/'desktop').chmod(0o755)
copy(root/'.local/media-tools/yt-dlp_linux',out/'tools/yt-dlp')
copy(root/'.local/media-tools/linux/deno',out/'tools/deno')
subprocess.run([args.godot,'--headless','--path',str(root/'project'),'--xr-mode','off','--export-pack','Linux',str(out/'prim.pck')],check=True)
# Keep machine paths in local build evidence, not the distributable manifest.
(root/'.local/linux-bundle-inputs.json').write_text(json.dumps({name:str(path) for name,path in dependencies.items()},indent=2))
(out/'runtime-libraries.json').write_text(json.dumps(sorted(dependencies),indent=2))
glibc_command = [sys.executable, str(root/'tools/package-linux-glibc.py'), '--game-dir', str(out)]
if args.glibc: glibc_command += ['--glibc', args.glibc]
subprocess.run(glibc_command, check=True)
print('Linux bundle:',out)

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
for source in (root / '.local/viseme-runtime/linux').iterdir():
    if source.name in ['LICENSE', 'ThirdPartyNotices.txt']:
        shutil.copy2(source, viseme_notices / ('onnxruntime-' + source.name))
