#!/usr/bin/env python3
"""Attach revision/notice manifests and an archive of tracked project sources."""
import argparse, io, json, shutil, subprocess, tarfile
from pathlib import Path
root=Path(__file__).resolve().parents[1]
p=argparse.ArgumentParser()
p.add_argument('--windows-build',type=Path,default=root/'.local/windows')
a=p.parse_args(); build=a.windows_build
source=root/'dist/prim-source';source.mkdir(parents=True,exist_ok=True)
def git(*args,cwd=root):return subprocess.check_output(['git',*args],cwd=cwd)
paths=['']+git('submodule','foreach','--quiet','--recursive','printf "%s\\n" "$displaypath"').decode().splitlines()
revisions={}
for relative in paths:
    repo=root/relative
    revisions[relative or 'prim']=git('rev-parse','HEAD',cwd=repo).decode().strip()
    destination=source/'prim'/relative;destination.mkdir(parents=True,exist_ok=True)
    with tarfile.open(fileobj=io.BytesIO(git('archive','HEAD',cwd=repo))) as archive:
        archive.extractall(destination,filter='data')
# The corresponding local patches are tracked in the video dependency.
for name in ['mpv','libplacebo']:
    repo=build/name
    if not repo.exists():continue
    destination=source/name;destination.mkdir(exist_ok=True)
    revisions[name]=git('rev-parse','HEAD',cwd=repo).decode().strip()
    with tarfile.open(fileobj=io.BytesIO(git('archive','HEAD',cwd=repo))) as archive:
        archive.extractall(destination,filter='data')
(source/'README.txt').write_text('Tracked prim sources and pinned mpv/libplacebo source references.\n'
 'Apply the patches under prim/dependencies/godot-libmpv-zero/patches to the corresponding upstream trees.\n'
 'Build recipes, pinned dependency versions and source URLs are included in prim/docs/BUILDING.md and the dependency Nix files.\n'
 'No lobby secret or machine configuration is included.\n')
for platform in ['linux','windows']:
    package=root/'dist'/('prim-'+platform)
    shutil.copy2(root/'docs/TESTING.md',package/'README.md')
    (package/'build-revisions.json').write_text(json.dumps(revisions,indent=2)+'\n')
    notices=package/'notices';notices.mkdir(exist_ok=True)
    for name in ['LICENSE.md','COPYRIGHT.txt']:
        if (build/'godot'/name).exists():shutil.copy2(build/'godot'/name,notices/('godot-'+name))
    for name in ['LICENSE.md','THIRDPARTY.md']:
        if (build/'sdk'/name).exists():shutil.copy2(build/'sdk'/name,notices/('steam-audio-'+name))
    for component, names in [('mpv', ['Copyright', 'LICENSE.GPL', 'LICENSE.LGPL']), ('libplacebo', ['LICENSE']), ('steam', ['LICENSE'])]:
        for name in names:
            if (build/component/name).exists(): shutil.copy2(build/component/name,notices/(component+'-'+name))
    shutil.copytree(build/'msys/mingw64/share/licenses',notices/'msys2',dirs_exist_ok=True)
    shutil.copy2(root/'build-support/windows-packages.json',notices/'windows-packages.json')
    shutil.copy2(root/'build-support/media-tools.json',notices/'media-tools.json')
    (package/'SOURCES.txt').write_text('Project source reference archive: prim-source.tar.gz, supplied beside these private builds.\n'
        'Godot/libmpv/libplacebo/Steam Audio patches and source pins are included there.\n'
        'Third-party package versions and notices are under notices/.\n')
with tarfile.open(root/'dist/prim-source.tar.gz','w:gz',compresslevel=3) as archive:
    archive.add(source,arcname='prim-source')
print('Attached build revisions, notices and tracked source reference archive.')
