#!/usr/bin/env python3
"""Fetch the pinned MSYS2 build sysroot before compiling patched libraries into it."""
import argparse, concurrent.futures, hashlib, json, subprocess, urllib.request
from pathlib import Path
root = Path(__file__).resolve().parents[1]
p = argparse.ArgumentParser()
p.add_argument('--directory', type=Path, default=root/'.local/windows')
p.add_argument('--download-only', action='store_true')
a = p.parse_args()
cache = a.directory/'packages'; cache.mkdir(parents=True, exist_ok=True)
stage = a.directory/'msys'; stage.mkdir(exist_ok=True)
manifest = root/'build-support/windows-packages.json'
marker = stage/'.prim-packages-sha256'
stamp = hashlib.sha256(manifest.read_bytes()).hexdigest()
if not a.download_only and marker.exists():
    if marker.read_text() != stamp: raise SystemExit('Use a fresh sysroot for the changed package lock.')
    print('Pinned sysroot already extracted; preserving locally built mpv/libplacebo.')
    raise SystemExit(0)
def fetch(package):
    target = cache/package['file']
    if not target.exists():
        temporary = target.with_suffix('.part')
        with urllib.request.urlopen('https://repo.msys2.org/mingw/mingw64/'+target.name) as source, temporary.open('wb') as output:
            while block := source.read(1024*1024): output.write(block)
        temporary.rename(target)
    if hashlib.sha256(target.read_bytes()).hexdigest() != package['sha256']:
        raise RuntimeError('Checksum mismatch: '+target.name)
    return target
with concurrent.futures.ThreadPoolExecutor(max_workers=4) as executor:
    for archive in executor.map(fetch, json.loads(manifest.read_text())):
        if not a.download_only:
            subprocess.run(['tar','--zstd','-xf',str(archive),'-C',str(stage),'--wildcards','mingw64/*'],check=True)
if not a.download_only: marker.write_text(stamp)
print('Pinned Windows packages verified.')
