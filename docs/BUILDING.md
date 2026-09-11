# Building prim

Use a native Linux filesystem for build outputs. Initialize submodules recursively.
The prototype expects the paired Godot build with the three interop/lifetime
patches in `dependencies/godot-libmpv-zero/patches/godot`; stock Godot is not a
supported runtime for this build.

## Linux

The video dependency's flake exports `godot-interop`, `mpv`, and `steam-audio`.
Build these once and retain their output paths; the prim development shell supplies
Cargo, CMake, Ninja, media fixture tools, bindgen, and Rust LLVM utilities.

```sh
nix build ./dependencies/godot-libmpv-zero#godot-interop -o .local/godot
nix build ./dependencies/godot-libmpv-zero#mpv -o .local/mpv
nix build ./dependencies/godot-libmpv-zero#mpv.dev -o .local/mpv-dev
nix build ./dependencies/godot-libmpv-zero#steam-audio -o .local/steam-audio
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

Generate the private lobby input once; distribute the same value in both builds:

```sh
python3 - <<'PY'
import json, os, secrets
path = 'project/private_lobby.json'
fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
with os.fdopen(fd, 'w') as f:
    json.dump({'secret': secrets.token_hex(32)}, f)
PY
```

Never commit that file. Export presets include it in private PCKs. The initial
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
2. Copy the pinned Godot 4.7.2 source from nixpkgs; apply the three Godot patches.
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
   hashes, class profile and four patches recorded in the video dependency's
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
