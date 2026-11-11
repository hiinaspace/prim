#!/usr/bin/env python3
"""Reusable Windows 11 VM for smoke-testing Prim Windows builds (user-space QEMU/KVM).

See docs/WINDOWS_VM.md. Typical use, on the Linux build host:
    tools/winvm.py install --iso ~/.cache/prim-winvm/iso/Win11_*.iso   # once, ~30-60 min
    tools/winvm.py smoke .local/dist/prim-windows-test.zip              # fresh VM each run
    tools/winvm.py start | ssh -- <cmd> | put | get | screenshot | stop | reset | status

No root, libvirt or system config: needs /dev/kvm (rw for your user) and Nix for
qemu, swtpm, OVMF and xorriso. Disks live in $PRIM_WINVM_DIR (default
~/.cache/prim-winvm, marked with CACHEDIR.TAG so Borg skips it).
"""
import argparse
import datetime
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
HERE = Path(__file__).resolve().parent / 'winvm'
STATE = Path(os.environ.get('PRIM_WINVM_DIR', Path.home() / '.cache/prim-winvm'))
BASE, RUN = STATE / 'base', STATE / 'run'
KEY = STATE / 'ssh/id_ed25519'
SSH_PORT = int(os.environ.get('PRIM_WINVM_SSH_PORT', '2222'))
VNC_DISPLAY = int(os.environ.get('PRIM_WINVM_VNC', '51'))  # 127.0.0.1:5951
MEMORY, CPUS = os.environ.get('PRIM_WINVM_MEM', '8G'), os.environ.get('PRIM_WINVM_CPUS', '4')
CACHEDIR_TAG = 'Signature: 8a477f597d28d172789f06886806bc55\n# prim-winvm: regenerable Windows VM images and disks\n'


def log(*a): print('[winvm]', *a, flush=True)


def nix_outs(attr):
    """Store paths of a nixpkgs attribute's outputs, built/fetched on demand and cached in STATE."""
    cache = STATE / 'nix-paths.json'
    paths = json.loads(cache.read_text()) if cache.exists() else {}
    if attr not in paths or not all(Path(p).exists() for p in paths[attr]):
        paths[attr] = subprocess.run(['nix', 'build', '--no-link', '--print-out-paths', f'nixpkgs#{attr}^*'],
                                     check=True, capture_output=True, text=True).stdout.split()
        cache.write_text(json.dumps(paths, indent=1))
    return [Path(p) for p in paths[attr]]


def find_in(attr, rel):
    for out in nix_outs(attr):
        if (out / rel).exists(): return out / rel
    raise SystemExit(f'nixpkgs#{attr} has no {rel}')


def tool(attr, exe): return str(find_in(attr, f'bin/{exe}'))
def ovmf(name): return find_in('OVMFFull', f'FV/{name}')


def ensure_state():
    STATE.mkdir(parents=True, exist_ok=True)
    tag = STATE / 'CACHEDIR.TAG'
    if not tag.exists(): tag.write_text(CACHEDIR_TAG)
    if not KEY.exists():
        KEY.parent.mkdir(exist_ok=True)
        subprocess.run(['ssh-keygen', '-q', '-t', 'ed25519', '-N', '', '-C', 'prim-winvm', '-f', str(KEY)], check=True)


# ---------------------------------------------------------------- QMP

class QMP:
    def __init__(self, path):
        self.s = socket.socket(socket.AF_UNIX)
        self.s.settimeout(30)
        for attempt in range(10):  # QEMU serves one QMP client at a time; a just-closed one may linger
            try: self.s.connect(str(path)); break
            except (ConnectionRefusedError, FileNotFoundError):
                if attempt == 9: raise
                time.sleep(1)
        self.f = self.s.makefile('rw')
        json.loads(self.f.readline())
        self.cmd('qmp_capabilities')

    def cmd(self, name, **args):
        self.f.write(json.dumps({'execute': name, 'arguments': args}) + '\n'); self.f.flush()
        while True:
            msg = json.loads(self.f.readline())
            if 'return' in msg: return msg['return']
            if 'error' in msg: raise RuntimeError(f'QMP {name}: {msg["error"]}')

    def close(self): self.f.close(); self.s.close()  # both, or QEMU still sees the client


def qmp(d=RUN): return QMP(d / 'qmp.sock')


# ---------------------------------------------------------------- VM lifecycle

def running(d=RUN):
    pid = d / 'qemu.pid'
    if not pid.exists(): return False
    try: os.kill(int(pid.read_text()), 0); return True
    except (ProcessLookupError, ValueError): return False


def start_swtpm(d):
    tpm = d / 'tpm'; tpm.mkdir(exist_ok=True)
    (tpm / 'sock').unlink(missing_ok=True)
    subprocess.run([tool('swtpm', 'swtpm'), 'socket', '--tpm2', '--tpmstate', f'dir={tpm}',
                    '--ctrl', f'type=unixio,path={tpm}/sock', '--daemon', '--terminate',
                    '--pid', f'file={d}/swtpm.pid', '--log', f'file={d}/swtpm.log'], check=True)
    for _ in range(50):
        if (tpm / 'sock').exists(): return
        time.sleep(0.1)
    raise SystemExit('swtpm did not create its socket')


def launch(d, cdroms=(), display=False):
    """Start QEMU (daemonized) for the VM directory d: disk.qcow2, vars.fd, tpm/."""
    start_swtpm(d)
    (d / 'qmp.sock').unlink(missing_ok=True)
    args = [tool('qemu_kvm', 'qemu-system-x86_64'), '-name', 'prim-win11',
            '-machine', 'q35,smm=on,accel=kvm', '-global', 'driver=cfi.pflash01,property=secure,value=on',
            '-cpu', 'host,hv_relaxed,hv_spinlocks=0x1fff,hv_vapic,hv_time',
            '-smp', CPUS, '-m', MEMORY, '-rtc', 'base=localtime',
            '-drive', f'if=pflash,format=raw,unit=0,readonly=on,file={ovmf("OVMF_CODE.ms.fd")}',
            '-drive', f'if=pflash,format=raw,unit=1,file={d}/vars.fd',
            '-chardev', f'socket,id=chrtpm,path={d}/tpm/sock', '-tpmdev', 'emulator,id=tpm0,chardev=chrtpm',
            '-device', 'tpm-tis,tpmdev=tpm0',
            # Emulated AHCI + e1000e + HDA: inbox Windows drivers, no virtio-win needed.
            '-device', 'ahci,id=ahci',
            '-drive', f'id=disk,if=none,format=qcow2,file={d}/disk.qcow2,discard=unmap',
            '-device', 'ide-hd,drive=disk,bus=ahci.0,bootindex=0',
            '-netdev', f'user,id=n0,hostfwd=tcp:127.0.0.1:{SSH_PORT}-:22', '-device', 'e1000e,netdev=n0',
            '-audiodev', 'none,id=snd0', '-device', 'ich9-intel-hda', '-device', 'hda-duplex,audiodev=snd0',
            '-device', 'qemu-xhci', '-device', 'usb-tablet', '-vga', 'std',
            '-vnc', f'127.0.0.1:{VNC_DISPLAY}', '-display', 'gtk' if display else 'none',
            '-qmp', f'unix:{d}/qmp.sock,server=on,wait=off',
            '-pidfile', f'{d}/qemu.pid', '-daemonize']
    for i, iso in enumerate(cdroms, start=1):
        args += ['-drive', f'id=cd{i},if=none,media=cdrom,readonly=on,file={iso}',
                 '-device', f'ide-cd,drive=cd{i},bus=ahci.{i},bootindex={i}']
    subprocess.run(args, check=True, stdout=open(d / 'qemu.log', 'a'), stderr=subprocess.STDOUT)
    log(f'QEMU up: VNC 127.0.0.1:{5900 + VNC_DISPLAY}, ssh 127.0.0.1:{SSH_PORT}')


def wait_exit(d, timeout):
    end = time.time() + timeout
    while time.time() < end:
        if not running(d): return True
        time.sleep(5)
    return False


def stop_vm(d=RUN, timeout=120):
    if not running(d): return
    try:
        q = qmp(d); q.cmd('system_powerdown'); q.close()
    except Exception as e: log('powerdown:', e)
    if not wait_exit(d, timeout):
        log('guest did not power off; quitting QEMU')
        try: q = qmp(d); q.cmd('quit'); q.close()
        except Exception: pass
        wait_exit(d, 15)


def screenshot(path, d=RUN):
    path = Path(path).resolve(); path.parent.mkdir(parents=True, exist_ok=True)
    q = qmp(d); q.cmd('screendump', filename=str(path), format='png'); q.close()
    return path


# ---------------------------------------------------------------- guest access

SSH_OPTS = ['-o', 'StrictHostKeyChecking=no', '-o', 'UserKnownHostsFile=/dev/null', '-o', 'LogLevel=ERROR',
            '-o', 'ConnectTimeout=5', '-o', 'BatchMode=yes', '-i', str(KEY)]


def ssh(command, check=True, **kw):
    return subprocess.run(['ssh', *SSH_OPTS, '-p', str(SSH_PORT), 'prim@127.0.0.1', command],
                          check=check, stdin=subprocess.DEVNULL, **kw)


def scp(src, dst):
    subprocess.run(['scp', '-q', *SSH_OPTS, '-P', str(SSH_PORT), src, dst], check=True)


def put(local, remote): scp(str(local), f'prim@127.0.0.1:{remote.replace(chr(92), "/")}')
def get(remote, local): scp(f'prim@127.0.0.1:{remote.replace(chr(92), "/")}', str(local))


def wait_ssh(timeout=900):
    end = time.time() + timeout
    while time.time() < end:
        if ssh('echo ok', check=False, capture_output=True, text=True).stdout.strip() == 'ok': return
        time.sleep(5)
    raise SystemExit(f'no ssh after {timeout}s; look at the VNC console or `winvm.py screenshot`')


# ---------------------------------------------------------------- commands

def cmd_install(a):
    ensure_state()
    if a.resume:
        if not running(BASE): raise SystemExit('no install VM running to resume')
        return wait_install(a)
    iso = Path(a.iso or '').resolve()
    if not a.iso or not iso.exists(): raise SystemExit(f'no ISO at {a.iso}')
    if running(BASE) or running(RUN): raise SystemExit('a VM is running; stop it first')
    if BASE.exists():
        if not a.force: raise SystemExit(f'{BASE} exists; pass --force to rebuild the base image')
        shutil.rmtree(BASE)
    BASE.mkdir()
    media = BASE / 'unattend'; media.mkdir()
    (media / 'autounattend.xml').write_text((HERE / 'autounattend.xml').read_text().replace('@EDITION@', a.edition))
    shutil.copy(HERE / 'prim-setup.ps1', media / 'prim-setup.ps1')
    shutil.copy(KEY.with_suffix('.pub'), media / 'authorized_keys')
    subprocess.run([tool('xorriso', 'xorriso'), '-as', 'mkisofs', '-quiet', '-J', '-r', '-V', 'PRIMSETUP',
                    '-o', str(BASE / 'unattend.iso'), str(media)], check=True)
    subprocess.run([tool('qemu_kvm', 'qemu-img'), 'create', '-q', '-f', 'qcow2', str(BASE / 'disk.qcow2'), f'{a.disk_gb}G'], check=True)
    shutil.copy(ovmf('OVMF_VARS.ms.fd'), BASE / 'vars.fd'); os.chmod(BASE / 'vars.fd', 0o644)
    (BASE / 'source').write_text(f'{iso.name}\n{a.edition}\n')
    launch(BASE, cdroms=[iso, BASE / 'unattend.iso'], display=a.display)
    # The Windows ISO's UEFI loader waits ~5 s for "Press any key to boot from CD", and
    # OVMF takes a variable time to get there. Tap 'x' (not Enter: once Setup is up,
    # Enter presses its Cancel button) until Setup starts writing the disk.
    disk = BASE / 'disk.qcow2'; size0 = disk.stat().st_blocks
    q = qmp(BASE)
    for _ in range(240):
        if disk.stat().st_blocks - size0 > 100 * 2048: break  # >100 MB written
        q.cmd('send-key', keys=[{'type': 'qcode', 'data': 'x'}]); time.sleep(0.5)
    q.close()
    wait_install(a)


def wait_install(a):
    log(f'installing; the guest powers itself off when setup is done (timeout {a.timeout}s)')
    t0 = time.time(); shots = BASE / 'install-shots'
    while running(BASE):
        if time.time() - t0 > a.timeout:
            screenshot(shots / 'timeout.png', BASE)
            raise SystemExit(f'install timed out; VM left running for inspection (VNC 127.0.0.1:{5900 + VNC_DISPLAY})')
        try: screenshot(shots / f'{int(time.time()):d}.png', BASE)
        except Exception as e: log('screenshot:', e)
        time.sleep(60)
    log(f'install VM powered off after {int(time.time() - t0)}s')
    (BASE / 'ready').write_text(datetime.datetime.now().isoformat() + '\n' + (BASE / 'source').read_text())


def fresh_run():
    if not (BASE / 'ready').exists(): raise SystemExit('no base image; run `winvm.py install` first')
    if running(RUN): stop_vm(RUN)
    shutil.rmtree(RUN, ignore_errors=True); RUN.mkdir()
    subprocess.run([tool('qemu_kvm', 'qemu-img'), 'create', '-q', '-f', 'qcow2', '-F', 'qcow2',
                    '-b', str(BASE / 'disk.qcow2'), str(RUN / 'disk.qcow2')], check=True)
    shutil.copy(BASE / 'vars.fd', RUN / 'vars.fd')
    shutil.copytree(BASE / 'tpm', RUN / 'tpm', ignore=shutil.ignore_patterns('sock'))


def cmd_start(a):
    ensure_state()
    if running(RUN): log('already running'); return
    if a.fresh or not (RUN / 'disk.qcow2').exists(): fresh_run()
    launch(RUN, display=a.display)
    wait_ssh(); log('ssh ready')


def cmd_stop(a): stop_vm(RUN)


def cmd_reset(a):
    stop_vm(RUN); shutil.rmtree(RUN, ignore_errors=True); log('run overlay discarded; next start is clean')


def cmd_status(a):
    print(json.dumps({'state_dir': str(STATE), 'base_ready': (BASE / 'ready').read_text().split('\n') if (BASE / 'ready').exists() else None,
                      'running': running(RUN), 'installing': running(BASE), 'ssh': f'127.0.0.1:{SSH_PORT}',
                      'vnc': f'127.0.0.1:{5900 + VNC_DISPLAY}'}, indent=1))


def cmd_ssh(a): sys.exit(ssh(' '.join(a.command), check=False).returncode)
def cmd_put(a): put(a.local, a.remote)
def cmd_get(a): get(a.remote, a.local)
def cmd_screenshot(a): print(screenshot(a.path))


def cmd_smoke(a):
    """Fresh VM, friend-style unzip, headless run, desktop run, screenshots and logs."""
    ensure_state()
    zip_path = Path(a.zip).resolve()
    out = Path(a.out or ROOT / '.local/winvm-smoke' / datetime.datetime.now().strftime('%Y%m%d-%H%M%S')).resolve()
    out.mkdir(parents=True)
    t0 = time.time(); steps = {}
    def step(name, fn):
        s = time.time()
        try: fn(); steps[name] = {'ok': True}
        except (Exception, SystemExit) as e: steps[name] = {'ok': False, 'error': str(e)}; log(name, 'FAILED:', e)
        steps[name]['seconds'] = round(time.time() - s, 1)
        return steps[name]['ok']
    def guest(phase, seconds=a.seconds):
        ssh(f'powershell -NoProfile -ExecutionPolicy Bypass -File C:/prim/guest-smoke.ps1 -Phase {phase} -Seconds {seconds}')

    fresh_run(); launch(RUN)
    try:
        if step('boot', wait_ssh):
            log('fresh VM up')
            ssh('New-Item -ItemType Directory -Force C:/prim | Out-Null')
            time.sleep(a.settle)  # let the desktop session finish logging in
            screenshot(out / '0-desktop.png')
            ok = step('upload', lambda: (put(HERE / 'guest-smoke.ps1', 'C:/prim/guest-smoke.ps1'), put(zip_path, 'C:/prim/package.zip')))
            ok = ok and step('unpack', lambda: guest('unpack'))
            if ok:
                step('headless', lambda: guest('headless'))
                if a.doubleclick:
                    step('doubleclick', lambda: guest('doubleclick-start'))
                    time.sleep(15); screenshot(out / '1-doubleclick.png')
                    ssh('Get-Process prim,cmd -ErrorAction SilentlyContinue | Stop-Process -Force', check=False)
                step('desktop', lambda: guest('desktop-start'))
                for i in range(3):
                    time.sleep(a.seconds / 3); screenshot(out / f'2-desktop-{i}.png')
                step('collect', lambda: guest('collect'))
                step('download', lambda: (get('C:/prim/out.zip', out / 'guest-out.zip'),
                                          shutil.unpack_archive(out / 'guest-out.zip', out / 'guest')))
    finally:
        if not steps.get('boot', {}).get('ok'): screenshot(out / 'boot-failed.png')
        stop_vm(RUN, timeout=60)
        if not a.keep: shutil.rmtree(RUN, ignore_errors=True)
    summary = {'zip': str(zip_path), 'base': (BASE / 'ready').read_text().split('\n')[:3],
               'seconds': round(time.time() - t0), 'steps': steps, 'out': str(out)}
    for name in ('unpack.json', 'headless.json', 'processes.json'):
        p = out / 'guest' / name
        if p.exists(): summary[name[:-5]] = json.loads(p.read_text(encoding='utf-8-sig'))
    (out / 'summary.json').write_text(json.dumps(summary, indent=1))
    print(json.dumps(summary, indent=1))
    print('WINVM_SMOKE_DONE' if all(s['ok'] for s in steps.values()) else 'WINVM_SMOKE_INCOMPLETE', out)


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest='cmd', required=True)
    s = sub.add_parser('install', help='build the clean base image from a Windows 11 ISO')
    s.add_argument('--iso')
    s.add_argument('--resume', action='store_true', help='keep waiting on an install VM that is already running')
    s.add_argument('--edition', default='Windows 11 Home', help='image name in install.wim (Home matches most friends)')
    s.add_argument('--disk-gb', type=int, default=64)
    s.add_argument('--timeout', type=int, default=5400)
    s.add_argument('--force', action='store_true'); s.add_argument('--display', action='store_true')
    s.set_defaults(fn=cmd_install)
    s = sub.add_parser('start', help='boot the run VM (overlay on the base) and wait for ssh')
    s.add_argument('--fresh', action='store_true', help='discard the overlay first'); s.add_argument('--display', action='store_true')
    s.set_defaults(fn=cmd_start)
    sub.add_parser('stop', help='ACPI power off (overlay kept)').set_defaults(fn=cmd_stop)
    sub.add_parser('reset', help='stop and discard the overlay (back to the clean base)').set_defaults(fn=cmd_reset)
    sub.add_parser('status').set_defaults(fn=cmd_status)
    s = sub.add_parser('ssh', help='run a PowerShell command in the guest'); s.add_argument('command', nargs=argparse.REMAINDER); s.set_defaults(fn=cmd_ssh)
    s = sub.add_parser('put'); s.add_argument('local'); s.add_argument('remote'); s.set_defaults(fn=cmd_put)
    s = sub.add_parser('get'); s.add_argument('remote'); s.add_argument('local'); s.set_defaults(fn=cmd_get)
    s = sub.add_parser('screenshot', help='PNG of the VM console'); s.add_argument('path'); s.set_defaults(fn=cmd_screenshot)
    s = sub.add_parser('smoke', help='fresh VM: unzip a Prim package, run headless + Desktop.bat, collect logs')
    s.add_argument('zip'); s.add_argument('--out'); s.add_argument('--seconds', type=int, default=45)
    s.add_argument('--settle', type=int, default=30)
    s.add_argument('--no-doubleclick', dest='doubleclick', action='store_false', help='skip the SmartScreen double-click screenshot')
    s.add_argument('--keep', action='store_true', help='keep the run overlay for inspection')
    s.set_defaults(fn=cmd_smoke)
    a = p.parse_args()
    if a.cmd == 'ssh' and a.command[:1] == ['--']: a.command = a.command[1:]
    a.fn(a)


if __name__ == '__main__':
    main()
