# prim launcher

Avalonia 12.1.2, self-contained .NET 10, Velopack 1.2.0. See
[the design and deferred multiplayer work](../docs/LAUNCHER_PLAN.md).

The friends deployment is live at https://prim.hiina.space/ (0.3.6 for Linux and Windows). See
[deployment details and the persistent signing-key location](hosting/DEPLOYMENT.md)
for subsequent releases. The generic key-generation example below is only for a
new deployment, not this existing feed.

## Build

The launcher consumes the existing platform game packages; it does not rebuild
Godot/native dependencies. Use matching fresh game exports for real releases.
The NuGet lockfile pins library dependencies. Build each platform sequentially.
The vpk CLI additionally needs a .NET 8 runtime on the build machine; clients do not.

On NixOS, from the repository root:

```sh
nix-build launcher/shell.nix -A runtime -o .local/launcher-runtime
nix-shell launcher/shell.nix
# Commands below run inside that shell.
dotnet build launcher/Prim.Launcher -c Release
dotnet launcher/Prim.Launcher/bin/Release/net10.0/PrimLauncher.dll --self-test
```

Create an RSA release key once in a private directory and back it up. Keep the
same key for this feed. Do not overwrite it each time you build. For example, for
a new deployment only, after choosing a previously unused key filename:

```sh
umask 077
mkdir -p .local/release-keys
# Only run key generation once; use a new filename if one already exists.
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:3072 \
  -out .local/release-keys/prim-release.pem
```

For each platform (`linux` then `windows`), build a new immutable release version.
Substitute the actual private feed prefix and notes file:

```sh
python3 tools/pack-launcher.py \
  --platform linux --version 0.3.1 \
  --game-dir dist/feedback-4/prim-linux \
  --feed-url https://file.hiina.space/PRIVATE-PREFIX/prim \
  --key .local/release-keys/prim-release.pem \
  --notes path/to/release-notes.md --output .local/launcher-production --fhs
```

On a conventional Linux build host, install the .NET 10 SDK, .NET 8 runtime,
Python, OpenSSL and squashfs tools, and omit `--fhs`. Cross-packing Windows works
on Linux; native Windows validation is still required. The output directory must
retain earlier full packages so vpk can create deltas. Never run two publishers
against it concurrently. `--output` selects a different staging root.

The command above creates `.local/launcher-production/{linux,windows}`. Keep this
production output separate from the existing localhost evaluation releases; never
publish those test versions as rollback candidates. Upload versioned
packages before the signed feed, and replace that feed atomically last. Publication
and OS code signing are separate; this script performs neither remote upload nor
Authenticode signing. See the plan for nginx cache policy.

## Run and install

Windows: run `Prim-friends-windows-Setup.exe` for a per-user installation. For a
custom directory, run the installer with `--installto "D:\Games\prim"`. A portable
ZIP is also built. Neither artifact currently removes unknown-publisher warnings.

Linux: mark `Prim-friends-linux.AppImage` executable and open it. The launcher has
user-folder and custom-folder install buttons. They copy the AppImage and create
a user desktop shortcut. Close the download copy and launch the installed copy.
The downloaded original is retained until you delete it. The app folder must be
user-writable and needs space for the installed image, cached full nupkg, extracted
runtime (without FUSE), and update staging; allow several GiB.

On NixOS, enter the shell above and start it with:

```sh
prim-launcher-fhs -c '/absolute/path/Prim-friends-linux.AppImage'
```

The install buttons preserve `PRIM_LAUNCHER_FHS` in an adjacent `run-prim` script;
the desktop shortcut invokes that script. Keep `.local/launcher-runtime` as a Nix
GC root or add the runtime to a persistent Nix configuration. Moving/deleting that
GC root is not a supported uninstall workflow. On other distributions lacking
usable FUSE, try `APPIMAGE_EXTRACT_AND_RUN=1` explicitly.

Preferences and logs live outside the application. Launcher state is under
`%LOCALAPPDATA%/prim/launcher` or `$XDG_DATA_HOME/prim/launcher` (default
`~/.local/share/prim/launcher`). Linux updates use `.prim-update` beside the
AppImage. Do not remove that directory while an update is running. Session logs,
reports and old detached helper copies currently require manual cleanup.

## Local evaluation and commands

Current evaluation builds intentionally use `http://127.0.0.1:18761`, with a test
key under the ignored `.local/launcher-keys` directory. They are **not friend-ready
downloads**. Serve their feed locally when checking them:

```sh
python3 -m http.server 18761 --bind 127.0.0.1 --directory .local/launcher-releases
```

Useful CLI commands on an installed package:

```text
--probe                         Print runtime, version, installation location
--self-test                     Signature/cache/redaction/path checks
--check                         Check signed feed
--prepare                       Download and verify available update
--verify                        Verify installed managed files (exit 2 on mismatch)
--report                        Export local scrubbed ZIP
--install-to /absolute/folder    Linux installation helper
--launch-smoke                  Desktop launch, quit after 90 frames, return child status
--update-and-restart /path.json  Apply update; restarted process writes a version probe
--repair-and-restart /path.json  Reinstall current release; write restart probe
--previous-and-restart /path.json Restore previous release; write restart probe
```

The last three are integration-test helpers that apply immediately; the GUI
requires a separate click after preparing. Normal GUI startup never installs a
prepared update automatically. Closing the launcher is blocked while its game or
an operation is active. A second launcher instance cannot own the same state.

## Verification evidence

Linux publishing restores Microsoft's portable .NET 10.0.11 packs instead of the
Nix SDK's installed packs. ELF interpreter/NEEDED/RPATH checks reject Nix
runtime dependencies before signing. The game's interpreter is unused (its
wrapper invokes the bundled loader), so its dynamic dependencies are checked
separately. A Nix-enabled Ubuntu machine can mask this failure; test with `/nix`
hidden too. From an isolated writable test directory:

```sh
bwrap --ro-bind / / --bind "$PWD" "$PWD" --tmpfs /nix --tmpfs /tmp \
  --dev-bind /dev /dev --proc /proc \
  --setenv XDG_DATA_HOME "$PWD/test-state" \
  --setenv APPIMAGE_EXTRACT_AND_RUN 1 ./Prim.AppImage --probe
```

The final Linux 0.3.3 package passed this isolation on Ubuntu 26.04.1, plus GUI,
file verification and desktop game startup/clean exit under an isolated Xvfb.
The old package fails the same startup test. See deployment notes for actual
VRM delta results and the remaining VR/hardware-driver test boundaries.

- Release build with warnings treated as errors; 23 self-checks covering signatures,
  changed metadata, channels, traversal, cache corruption, recompressed/tampered ZIP
  entries and secret canaries, including preserving timestamps.
- Actual NixOS self-contained AppImage execution and Avalonia/Xvfb window.
- Full repair with detached-helper restart; delta update 0.2.6 to 0.2.7 with
  authenticated reconstructed contents and restart into 0.2.7; previous-version
  restoration to 0.2.6. Installed-file verification passed after updating.
- Corrupted cached-package repair, an actual support-ZIP canary check, and a
  custom install path containing spaces (including its Nix wrapper outside nix-shell).
- Packaged desktop game smoke run: exit 0 after 90 frames; logs show Godot Vulkan
  initialization on NVIDIA, Steam Audio initialization and the bundled libmpv path.
  This does not validate video playback, voice or VR.
- Linux full package approximately 351 MiB; one small-change delta approximately
  212 KiB. Final sizes vary by source version.
- Windows self-contained cross-publish and Velopack Setup/portable/delta packaging.
  The portable runtime probe ran under Wine 11.12 and reported .NET 10.0.11;
  the final 0.3.0 Windows portable build passed all 23 self-checks under Wine.
  Setup is a 32-bit bootstrap; this Wine prefix lacks its 32-bit kernel32.dll,
  so native Windows install/update/repair remains unverified.

Desktop rendering, VR/headset/input/voice and native Windows behavior are separate
acceptance gates. The production feed and nginx vhost are deployed; see `hosting/DEPLOYMENT.md`.
