# Building prim

The supported helper targets Linux x86-64. Install Git and Nix with the `nix-command`
and `flakes` experimental features enabled (for example, `experimental-features =
nix-command flakes` in your Nix configuration). Use a native Linux filesystem for
build outputs and a Vulkan-capable driver for the rendered application. A headset
is optional for desktop development; audio tests also need a PulseAudio-compatible
server. The build downloads pinned sources and runtimes from the network.

Once repository access is available:

```sh
git clone --recurse-submodules https://github.com/hiinaspace/prim.git
cd prim
./build.sh
./run.sh --desktop
```

For an existing clone, use `git submodule update --init --recursive` to initialize
missing dependencies. Source builds require no production signing key, maintainer
notes or prebuilt friends package. Initial build time/disk requirements still need
measurement in the [publication clean-build pass](PUBLICATION_PLAN.md).

The prototype expects the paired Godot build with the three interop/lifetime
patches in `dependencies/godot-libmpv-zero/patches/godot` plus
`build-support/godot/0004-restartable-openxr.patch`,
`build-support/godot/0005-restore-stereo-on-xr-reentry.patch`,
`build-support/godot/0006-companion-overlay.patch`, and
`build-support/godot/0007-steamvr-vulkan-interop.patch`; stock Godot is not a
supported runtime for this build.

## Local build and launch helper

On Linux x86-64 with Nix, run `./build.sh` from this checkout (or invoke it by
absolute path). It enters the project's development shell, initializes missing
submodules, resolves the patched Godot and Steam Audio builds, builds missing mpv dependencies, builds
and stages the C++ and Rust extensions, fetches verified media and viseme
runtimes, then imports the Godot project. `JOBS=8 ./build.sh` changes C++ build
parallelism. Existing dependency working trees and lobby secrets are preserved.
Godot and Steam Audio derivations are always resolved so new local patches cannot
be skipped; unchanged Nix outputs are reused. Other staged dependencies are reused; use
`./build.sh --refresh-deps` to rebuild them from the pinned flake after dependency
changes. A first build or dependency refresh can take substantially longer.

Run `./run.sh` to try VR with a desktop fallback, `./run.sh --desktop`
to start in desktop mode (Enter VR remains available), or `./run.sh --editor` for the patched editor. The launcher sets
libmpv and media-helper paths and enters the same Nix shell. It does not rebuild;
repeat `./build.sh` after native changes. GDScript changes need only a restart.
Use `./run.sh --desktop --test-vrm /path/model.vrm` for an isolated offline
private-avatar comparison; see [avatars](AVATARS.md#private-vrm-comparison-local-development-only).
For the runtime-import proof (original bytes, no editor import), use
`./run.sh --desktop --runtime-vrm /path/model.vrm`. This previews the file offline;
choose a bundled avatar to connect again. See [avatars](AVATARS.md#runtime-vrm-import-proof).
Additional arguments go to Godot, for example:

```sh
./run.sh --desktop --headless --quit-after 90
```

A new checkout gets a random private lobby secret if none exists. Peers must
share that same private file to discover each other. These helpers build a local
Linux development tree; distribution packaging and Windows builds remain the
separate workflows below.

## Linux

The video dependency's flake exports `godot-interop`, `mpv`, and `steam-audio`.
Build these once and retain their output paths; the prim development shell supplies
Cargo, CMake, Ninja, media fixture tools, bindgen, and Rust LLVM utilities.

```sh
./tools/build-godot.sh
nix build ./dependencies/godot-libmpv-zero#mpv -o .local/mpv
nix build ./dependencies/godot-libmpv-zero#mpv.dev -o .local/mpv
./tools/build-steam-audio.sh
nix develop
cargo build
cmake -S dependencies/godot-libmpv-zero -B .local/video-build -G Ninja \
  -DMPV_DIR="$PWD/.local/mpv-dev" -DCMAKE_BUILD_TYPE=Release
cmake --build .local/video-build -j4
```

Stage the video GDExtension descriptor and compiled library into `project/bin`
and `project/bin/linux`, respectively. Put the Steam Audio output (its `bin` and
`icons` directories) under `project/addons/godot-steam-audio`. The native prim
library is `target/debug/libprim_native.so`; `tools/stage-linux.sh` can also copy
these from an already-staged video demo using `PRIM_VIDEO_PROJECT`.

For an isolated development room, generate the lobby input once; distribute the
same value privately to the builds participating in that test:

```sh
python3 - <<'PY'
import json, os, secrets
path = 'project/private_lobby.json'
fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
with os.fdopen(fd, 'w') as f:
    json.dump({'secret': secrets.token_hex(32)}, f)
PY
```

Do not commit that local file. Export presets include it in PCKs, so anyone with
such a package can obtain its room configuration; this is not a private-membership
guarantee for the official shared lobby. See [lobby policy](../CONTRIBUTING.md#releases-compatibility-and-the-shared-lobby).
The initial
native descriptor is created by `tools/stage-linux.sh`; compatibility is Godot 4.7.

```sh
python3 tools/fetch-media-tools.py
python3 tools/package-linux.py --godot "$GODOT" --mpv "$LIBMPV_ZERO_MPV_LIBRARY"
```

Set `GODOT` to the patched editor executable and `LIBMPV_ZERO_MPV_LIBRARY` to the
patched shared library. These are local environment settings, not project data.
The Linux bundler copies ELF dependencies and launches through its included glibc
loader; graphics drivers and the OpenXR runtime still come from the host. It has
been tested locally, not across every Linux distribution.

## Windows cross-build

The initial cross-build uses MinGW-w64 GCC 15.3 with mcfgthread 2.4.2, Rust 1.96.0
with `x86_64-pc-windows-gnu`, and the pinned MSYS2 package set in
`build-support/windows-packages.json`. `tools/fetch-windows-packages.py` verifies
and extracts that sysroot under `.local/windows/msys`. Run it **before** installing
the patched libplacebo/mpv into the same sysroot. Retain the package cache.

These are the tested recipe stages; compiler paths are supplied locally:

1. Use the pinned nixpkgs `pkgsCross.mingwW64.stdenv.cc` and
   `pkgsCross.mingwW64.windows.mcfgthreads`. Provide the latter's include/lib
   directories to the cross compiler. The compiler prefix must include `gcc-ar`
   and `gcc-ranlib` from the underlying GCC package as well as the normal tools.
   Cross compiler wrappers must clear the host `NIX_CFLAGS_COMPILE` and
   `NIX_LDFLAGS`; add the Windows sysroot's include/lib directories instead.
2. Copy the pinned Godot 4.7.2 source from nixpkgs; apply the three media Godot patches,
   then `build-support/godot/0004-restartable-openxr.patch`,
   `build-support/godot/0005-restore-stereo-on-xr-reentry.patch`,
   `build-support/godot/0006-companion-overlay.patch`, and
   `build-support/godot/0007-steamvr-vulkan-interop.patch`. The overlay hook is optional
   at runtime; SteamVR continues to use an ordinary scene session.
   Build with `scons platform=windows arch=x86_64 target=template_debug
   use_mingw=yes use_static_cpp=yes d3d12=no opengl3=yes debug_symbols=no
   lto=none -j4`, with `MINGW_PREFIX` pointing to the cross toolchain.
   **Keep OpenGL enabled** even though prim uses Vulkan: removing it changes the
   generated class API (notably WebXR) expected by the Rust bindings.
3. Build libplacebo v7.360.1 with the dependency's libplacebo patch, recursively
   initialized source submodules, shared libraries, Vulkan/shaderc enabled,
   OpenGL/D3D11 disabled, and xxhash disabled. The latter avoids an unsysrooted
   pkg-config include path in this cross setup. Install under `/mingw64` using
   the MSYS staging directory as `DESTDIR`.
4. Build the mpv fork at `970250ad1480e78ca03999e1733d212278f07dbb` plus all four
   patches in `patches/mpv`. Meson options: `--prefix /mingw64 --libdir lib
   -Ddefault_library=shared -Dbuildtype=release -Dauto_features=disabled
   -Dlibmpv=true -Dcplayer=false -Dlua=luajit -Dgl=disabled -Dvulkan=enabled
   -Dshaderc=enabled -Dwin32-threads=enabled -Diconv=enabled -Dzlib=enabled`.
   Use a Meson Windows cross file with the cross compilers, pkg-config sysroot,
   and explicit Windows include/library directories. Install into the same sysroot.
5. Build godot-steam-audio `8f65c29b21c1d8cdbf2d6dbfc53c92ef95dd2a93`, SDK 4.8.1,
   and godot-cpp `4862a9dcf1471c9ea19680b9faadb5b6a9432092` using the sources,
   hashes, class profile and five patches recorded in the video dependency's
   `nix/steam-audio.nix`. Its `steam-audio-CMakeLists.txt` now supports Windows.
   Set `GODOT_CPP_SOURCE`, `STEAM_AUDIO_SDK`, `GODOTCPP_BUILD_PROFILE`, and a
   Windows CMake toolchain; link the GCC C++ runtime statically.
6. Build libmpv-zero with that CMake toolchain, the fork's development headers and
   the Windows Vulkan headers. Build prim with `cargo build --target
   x86_64-pc-windows-gnu`, setting the target linker, `CC_x86_64_pc_windows_gnu`
   and `AR_x86_64_pc_windows_gnu`. `.cargo/config.toml` disables MinGW's implicit
   auto-export: otherwise an intermediate Iroh DLL exceeds the PE export limit.
   Put Cargo output under `.local/windows/rust-build`.
7. `tools/package-windows.py` collects the engine, three extensions, mpv, Steam
   Audio SDK and transitive DLL imports from those build directories. Supply
   `--godot` (the Linux editor for export), `--objdump`, and any additional
   `--runtime-directory` containing compiler runtime DLLs. Pinned yt-dlp, Deno
   and the Microsoft runtime installer come from `tools/fetch-media-tools.py`.

The layout expected by the packaging tool is explicit in its seed map. This is a
working prototype recipe, not yet a single clean-room Windows CI derivation.
The Windows launchers set the working directory and helper-tool PATH explicitly.

## Checks

```sh
cargo test
cargo clippy --all-targets -- -D warnings
GODOT="$GODOT" python3 tools/test-integration.py
PRIM_TEST_PEERS=5 GODOT="$GODOT" python3 tools/test-integration.py
cargo test public_dht_discovers_host_without_address_exchange -- --ignored
```

The process integration test creates its own PulseAudio sinks and synthetic input;
it does not switch the desktop's audio defaults. It covers six peers, real video,
voice, pose, pause/seek/resume, mute/unmute and a 700 ms Godot main-thread stall.
The public DHT test contacts the public network with a fresh ephemeral test room.

Export templates disable external `--script` overrides. For packaged tests use
`tools/export-test-pack.py --platform Windows --script tests/package_smoke.gd
--output <test-package>/prim.pck --godot "$GODOT"`, keeping the regular PCK intact.
For a mixed integration test, export `tests/integration.gd` instead and set
`PRIM_TEST_WINDOWS_EXE` to that package's executable, plus an isolated
`WINEPREFIX`, when running `tools/test-integration.py`. Wine must use its builtin
Vulkan loader (`WINEDLLOVERRIDES=vulkan-1=b`); the runner sets this for its client.
Native Windows uses the normal Windows loader.

## Voice viseme runtime and checks

Before packaging either platform, stage the hash-pinned CPU ONNX Runtime:

```sh
python3 tools/fetch-viseme-runtime.py --platform all
cargo build
cp target/debug/libprim_native.so project/bin/linux/
```

The model is embedded in the native extension. Keep `native/viseme-model` notices
with distributions; packaging copies these and ONNX Runtime notices automatically.
The runtime can be overridden for diagnostics with `PRIM_ONNXRUNTIME_LIBRARY`.

```sh
python3 tools/fetch-viseme-fixture.py # requires ffmpeg; media stays in .local
PRIM_TEST_SPEECH_PCM="$PWD/.local/viseme/speech.f32" "$GODOT" --headless --path project --xr-mode off --script res://tests/visemes.gd
PRIM_VISEME_PREVIEW="$PWD/.local/viseme-preview.png" "$GODOT" --path project --xr-mode off --script res://tests/viseme_preview.gd
cargo build --example viseme_probe
python3 tools/test-viseme-reference.py .local/viseme/speech.f32 --rate 48000 --runtime project/bin/linux/libonnxruntime.so --out .local/viseme/parity-48000
```

The reference check requires NumPy and Python ONNX Runtime. Repeat with the
fixture's 16000 and 44100 Hz files. For the missing-runtime case add
`PRIM_TEST_NO_ORT=1 PRIM_ONNXRUNTIME_LIBRARY=/nonexistent/onnxruntime.so` to the
speech test. The regular process integration test also checks live visemes.
For packaged tests export `tests/visemes.gd` with `export-test-pack.py`, replace
`prim.pck` in a disposable test bundle, and pass the PCM fixture environment
variable (a Windows path under Wine). Templates may reject `--main-pack`.

### SteamVR background helper

`./build.sh` stages the Linux helper through `tools/build-openvr-helper.sh linux`.
For Windows, run `tools/build-openvr-helper.sh windows`; it stages the helper EXE,
OpenVR DLL and SDK license under `project/bin/windows`. Both targets use the
pinned nixpkgs OpenVR source. Windows links its C++/thread support statically.
The packagers require the helper, copy its license and include its runtime
libraries. Linux's helper uses the same private glibc loader as the main app.

The Windows Godot build needs `MINGW_PREFIX` to name a toolchain **directory**
containing `bin/x86_64-w64-mingw32-*`, not an executable prefix. Put that bin
folder on PATH as well, since windres and gcc-ar invoke companion tools. Ensure
its gcc/g++ entries use the wrappers from step 1 above; SCons selects them from
the prefix and can override command-line CC/CXX assignments.

### Experimental OpenVR peek extension

`build.sh` now includes `tools/build-openvr-overlay.sh linux`. The helper pins
upstream plus Prim's lifecycle patch and stages the extension next to the native
libraries. Windows: run `tools/build-openvr-overlay.sh windows` after preparing
the existing engine cross toolchain, then the normal Windows packager. Override
`MINGW_PREFIX` for another toolchain location. Action/binding JSON is explicitly
included in both export presets and extracted to a writable per-user directory
when SteamVR starts; do not rely on source-checkout paths in a packaged build.
See [the patch/build notes](../build-support/openvr-overlay/README.md).
