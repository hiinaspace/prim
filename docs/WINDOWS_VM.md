# Windows 11 smoke-test VM

`tools/winvm.py` runs a real Windows 11 in user-space QEMU/KVM on the Linux build host, so a Windows
package can be checked on Windows and not only under Wine. Every smoke run starts
from a clean snapshot. It is a desktop smoke test: there is no GPU passthrough, VR
or real microphone.

## Requirements

- `/dev/kvm` readable and writable by you. On the NixOS build host it is
  `crw-rw-rw-`, so no root, libvirt or NixOS change is needed.
- Nix. The tool pulls `qemu_kvm`, `swtpm`, `OVMFFull` (Secure Boot firmware with
  Microsoft keys) and `xorriso` from nixpkgs and caches their store paths.
- About 40 GB free. Everything bulky lives in `$PRIM_WINVM_DIR` (default
  `~/.cache/prim-winvm`), which carries a `CACHEDIR.TAG` so Borg skips it.
- A Windows 11 x64 ISO. Microsoft blocks scripted downloads from the build host's IP, so get
  one from <https://www.microsoft.com/en-us/software-download/windows11> in a
  browser. Pick "multi-edition ISO", then English (United States). The page gives a
  24-hour link that `curl` can fetch on the build host. Check the SHA-256 against the table on
  that page. The current base was built from `Win11_26300_en-us_x64.iso`, sha256
  `bd4307df…ea650`.

## Build the base image (once, ~30–60 min)

```
uv run --no-project tools/winvm.py install --iso ~/.cache/prim-winvm/iso/Win11_26300_en-us_x64.iso
```

This installs **Windows 11 Home** (`--edition "Windows 11 Pro"` also works) without
supervision and with no product key. Windows runs unactivated, which shows a
watermark but doesn't expire. The install uses `tools/winvm/autounattend.xml`:

- UEFI with Secure Boot and a software TPM 2.0 (swtpm), as on a friend's PC.
- A local admin account, `prim`, with password `prim`. It's a throwaway login and
  the VM is reachable only on 127.0.0.1. Autologon gives every boot a real desktop
  session.
- Automatic BitLocker is disabled, because a recovery prompt would wedge a reset VM.

On first logon, `tools/winvm/prim-setup.ps1` installs OpenSSH Server, adds the
tool's key (`$PRIM_WINVM_DIR/ssh/id_ed25519`), turns off sleep and screen blanking,
records OS, GPU, audio and Smart App Control facts in `C:\prim-vm-info.json`, and
then powers off. Defender, SmartScreen and Smart App Control stay as shipped.
Progress screenshots go to `base/install-shots/`. If the tool dies while the
install VM is still running, re-attach with `install --resume`.

The ISO's "Press any key to boot from CD" prompt is answered by tapping `x` over
QMP until Setup starts writing the disk. Enter is not used, because once Setup is up,
Enter presses its Cancel button.

## Smoke-test a package

```
uv run --no-project tools/winvm.py smoke ~/code/prim-identity/.local/dist/prim-windows-test.zip
```

Each smoke run takes these steps:

1. Creates a fresh qcow2 overlay on the base, with copies of the UEFI variables
   and TPM state, then boots and waits for ssh.
2. Uploads the zip and unpacks it the way a friend would get it. The zip and every
   extracted file get the Mark-of-the-Web (`Zone.Identifier`, ZoneId=3), as Explorer
   does for a browser or Discord download. It also records the OS build, Smart App
   Control state, video adapter, Vulkan ICDs and audio devices.
3. **headless**: runs `prim.exe --headless --xr-mode off -- --desktop` over ssh for
   `--seconds`. This checks DLL loading, Rust and `identity.key`.
4. **doubleclick**: ShellExecutes `Desktop.bat` in the logged-on desktop session,
   the path a double-click takes. It screenshots whatever SmartScreen or Smart App
   Control shows (`1-doubleclick.png`), then kills it.
5. **desktop**: runs `Desktop.bat` through cmd in the desktop session (an
   interactive scheduled task), so SmartScreen doesn't hold it. It takes three
   screenshots (`2-desktop-*.png`).
6. **collect**: saves the process list, `%APPDATA%\prim` and Godot user logs,
   Application event-log errors, and WER crash reports. Then it powers off and
   discards the overlay. Pass `--keep` to keep the overlay.

Results go to `.local/winvm-smoke/<timestamp>/`: `summary.json`, the screenshots,
and `guest/` (stdout/stderr, logs, events). The run prints `WINVM_SMOKE_DONE` when
every step ran. That means the harness worked, not that Prim did. Read the
screenshots and `processes.json`.

## Driving the VM by hand

```
tools/winvm.py start [--fresh]        # boot (overlay kept between starts), wait for ssh
tools/winvm.py ssh -- Get-Process     # PowerShell in the guest (non-interactive session 0-style)
tools/winvm.py put file C:/prim/file  # scp
tools/winvm.py get C:/prim/out.zip .
tools/winvm.py screenshot shot.png    # QMP screendump of the console
tools/winvm.py stop | reset | status
```

To watch live, use VNC on `127.0.0.1:5951` on the build host (forward it with
`ssh -L 5951:127.0.0.1:5951 <host>`), or pass `--display` for a GTK window there.

## Rendering and Vulkan in the VM

The guest has QEMU's `-vga std` with the Microsoft Basic Display Adapter. It has
no Vulkan ICD and no hardware OpenGL or D3D. D3D12 can run only on WARP, Windows'
software rasteriser. A smoke run shows whether Prim's renderer starts or falls
back on such a machine. It says nothing about rendering on a real GPU. Options for
later, none set up: a Mesa lavapipe ICD in the guest (software Vulkan), or
passthrough of a spare GPU.

## Toward `tools/test-friend-join.py` on real Windows

`test-friend-join.py` can already run one role as a Windows exe under Wine
(`PRIM_TEST_WINDOWS_EXE`). Moving that role into the VM needs three things:

- the export test pack (`tools/export-test-pack.py`), not the friend zip, because
  the role needs `tests/friend_join.gd`;
- a launcher in that script that does `put`, then `ssh` with the role's
  environment and `PRIM_TEST_DIR=C:\...`, then `get` for the role's `.json`/`.id`;
- a network path. The test pins `PRIM_NETWORK_LOCAL_ONLY` (loopback), and the VM
  sits behind QEMU user-mode NAT: the host is `10.0.2.2` from the guest, and
  host-to-guest works only through `hostfwd` ports. Direct UDP between host and
  guest roles needs either per-port `hostfwd=udp:` forwards or a tap/bridge
  (bridging needs root). Going through the relay or internet path avoids both.

## First smoke run, 2026-11-11

This run used the identity round 1 Windows test zip (`prim-windows-test.zip`,
`scripts/build-windows.sh` on `claude/windows-test-build`). The base was Windows 11
Home build 26300, unactivated. The run took 196 s and every harness step completed.
The evidence is in `.local/winvm-smoke/20261111-145152/` on the build host, which isn't committed.

- **Headless:** boots and stays up for 45 s. godot-rust, Steam Audio (Embree)
  and libmpv load, and `%APPDATA%\prim\identity.key` is written.
- **Lip sync fails on a fresh Windows:** `onnxruntime.dll` fails with
  `LoadLibraryExW failed`. That Windows has no VC++ 2015–2022 runtime (no
  `msvcp140.dll` or `vcruntime140_1.dll` in System32). After the bundled
  `VC_redist.x64.exe /install /quiet` the warning is gone. Prim keeps running
  (voice stays on), so the README's "if a DLL is missing" hint never fires. Wine
  didn't show this.
- **Double-click:** `Desktop.bat` carries Mark-of-the-Web, and opening it shows
  "Open File – Security Warning", Unknown Publisher, with **Run** and Cancel
  buttons. That's the Attachment Manager prompt for a .bat, not the SmartScreen
  "More info → Run anyway" page the README describes. Smart App Control is in
  evaluation mode (state 2), so it neither blocked nor prompted.
- **Desktop rendering:** Vulkan fails (no ICD, `VK_KHR_surface` missing), then
  D3D12 is reported unsupported. Godot falls back to OpenGL 3, which fails to
  create a window, and it shows the "Unable to initialize video driver" dialog.
  That is expected on a VM without a GPU and says nothing about real GPUs. A
  software Vulkan (lavapipe) in the guest would let the desktop path render.

Not covered: a real GPU, VR, a microphone, the Windows Firewall prompt (no
listening role ran in the desktop session long enough to trigger it), the
internet relay, Windows 10.
