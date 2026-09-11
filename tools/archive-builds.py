#!/usr/bin/env python3
"""Create friend-test archives, accepting reproducible-build epoch timestamps."""
import hashlib, subprocess, zipfile
from pathlib import Path
root=Path(__file__).resolve().parents[1]/'dist'
with zipfile.ZipFile(root/'prim-windows.zip','w',zipfile.ZIP_DEFLATED,compresslevel=3,strict_timestamps=False) as archive:
    for path in sorted((root/'prim-windows').rglob('*')):
        if path.is_file(): archive.write(path,path.relative_to(root))
subprocess.run(['tar','--use-compress-program=gzip -3','-cf',str(root/'prim-linux.tar.gz'),'-C',str(root),'prim-linux'],check=True)
lines=[]
for name in ['prim-linux.tar.gz','prim-windows.zip','prim-source.tar.gz']:
    with (root/name).open('rb') as source: digest=hashlib.file_digest(source,'sha256').hexdigest()
    lines.append(digest+'  '+name)
    print(name,round((root/name).stat().st_size/1024/1024),'MiB')
(root/'SHA256SUMS').write_text('\n'.join(lines)+'\n')
