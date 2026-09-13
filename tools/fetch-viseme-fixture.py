#!/usr/bin/env python3
"""Fetch one attributed LibriSpeech test clip and convert to mono PCM for viseme tests.
Requires ffmpeg. Streams the archive only up to the requested member; keeps all
media in ignored .local, outside the game distribution.
"""
import hashlib
import json
from pathlib import Path
import subprocess
import tarfile
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
URL = 'https://www.openslr.org/resources/12/test-clean.tar.gz'
MEMBER = 'LibriSpeech/test-clean/6930/75918/6930-75918-0000.flac'
SHA = '9ce35224156f071ab58eb7feb8a5ceae600f6f9f353da2a6cbf797b6b1ac8a23'
out = ROOT / '.local/viseme'
out.mkdir(parents=True, exist_ok=True)
clip = out / 'speech.flac'
if not clip.exists():
    with urllib.request.urlopen(URL, timeout=60) as response:
        with tarfile.open(fileobj=response, mode='r|gz') as archive:
            for member in archive:
                if member.name == MEMBER:
                    data = archive.extractfile(member).read()
                    if hashlib.sha256(data).hexdigest() != SHA:
                        raise RuntimeError('Speech fixture hash mismatch')
                    clip.write_bytes(data)
                    break
            else:
                raise RuntimeError('Speech fixture missing from archive')
if hashlib.sha256(clip.read_bytes()).hexdigest() != SHA:
    raise RuntimeError('Cached speech fixture hash mismatch')
(out / 'speech-source.json').write_text(json.dumps({
    'url': URL, 'member': MEMBER, 'sha256': SHA, 'license': 'CC BY 4.0',
    'attribution': 'LibriSpeech ASR corpus, Vassil Panayotov, Guoguo Chen, Daniel Povey, Sanjeev Khudanpur; recordings from LibriVox',
}, indent=2) + '\n')
for rate in [16000, 44100, 48000]:
    destination = out / ('speech.f32' if rate == 48000 else f'speech-{rate}.f32')
    subprocess.run(['ffmpeg', '-nostdin', '-v', 'error', '-y', '-i', str(clip),
                    '-af', 'apad=pad_dur=1', '-ac', '1', '-ar', str(rate),
                    '-f', 'f32le', str(destination)], check=True)
print(out / 'speech.f32')
