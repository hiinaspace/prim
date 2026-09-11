#!/usr/bin/env python3
import hashlib, json, urllib.request, zipfile
from pathlib import Path
root=Path(__file__).resolve().parents[1]
out=root/'.local/media-tools'
out.mkdir(parents=True,exist_ok=True)
for asset in json.loads((root/'build-support/media-tools.json').read_text()):
    path=out/asset['name']
    if not path.exists():
        with urllib.request.urlopen(asset['url']) as source, path.open('wb') as destination:
            while chunk:=source.read(1024*1024): destination.write(chunk)
    assert hashlib.sha256(path.read_bytes()).hexdigest()==asset['sha256'], asset['name']
    if path.suffix=='.zip':
        platform='windows' if 'windows' in path.name else 'linux'
        with zipfile.ZipFile(path) as archive: archive.extractall(out/platform)
    else: path.chmod(0o755)
for path in out.glob('*/deno*'): path.chmod(0o755)
print('Pinned yt-dlp and Deno assets verified.')
