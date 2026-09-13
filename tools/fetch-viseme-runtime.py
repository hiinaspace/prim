#!/usr/bin/env python3
"""Fetch pinned CPU ONNX Runtime archives; stage only native runtime and notices."""
import argparse, hashlib, io, shutil, tarfile, urllib.request, zipfile
from pathlib import Path
ROOT = Path(__file__).resolve().parents[1]
VERSION = '1.26.0'
ARTIFACTS = {
    'linux': ('onnxruntime-linux-x64-1.26.0.tgz', '1254da24fb389cf39dc0ff3451ab48301740ffbfcbaf646849df92f80ee92c57'),
    'windows': ('onnxruntime-win-x64-1.26.0.zip', '6ebe99b5564bf4d029b6e93eac9ff423682b6212eade769e9ca3f685eaf500b4'),
}
def fetch(platform):
    name, digest = ARTIFACTS[platform]
    directory = ROOT / '.local/viseme-runtime' / platform
    directory.mkdir(parents=True, exist_ok=True)
    archive = directory.parent / name
    if not archive.exists():
        data = urllib.request.urlopen(f'https://github.com/microsoft/onnxruntime/releases/download/v{VERSION}/{name}', timeout=60).read()
        if hashlib.sha256(data).hexdigest() != digest: raise RuntimeError('Archive checksum mismatch')
        archive.write_bytes(data)
    data = archive.read_bytes()
    if hashlib.sha256(data).hexdigest() != digest: raise RuntimeError(f'Corrupt cached archive: {archive}')
    if platform == 'linux':
        with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as source:
            files = {Path(m.name).name: source.extractfile(m).read() for m in source if m.isfile() and (m.name.endswith('.so') or m.name.endswith('.so.'+VERSION) or Path(m.name).name in ['LICENSE', 'ThirdPartyNotices.txt'])}
        files['libonnxruntime.so'] = files.pop('libonnxruntime.so.'+VERSION)
    else:
        with zipfile.ZipFile(io.BytesIO(data)) as source:
            files = {Path(n).name: source.read(n) for n in source.namelist() if n.endswith('.dll') or Path(n).name in ['LICENSE', 'ThirdPartyNotices.txt']}
    for name, content in files.items(): (directory / name).write_bytes(content)
    staged = ROOT / 'project/bin' / platform
    staged.mkdir(parents=True, exist_ok=True)
    for path in directory.iterdir():
        if path.suffix not in ['.so', '.dll']: continue
        target = staged / path.name
        if target.is_symlink(): target.unlink()
        shutil.copy2(path, target)
    print(f'ONNX Runtime {VERSION} ({platform}): {directory}')
if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--platform', choices=['linux','windows','all'], default='all')
    args = p.parse_args()
    for platform in ARTIFACTS if args.platform == 'all' else [args.platform]: fetch(platform)
