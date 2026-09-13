#!/usr/bin/env bash
# Build and stage a local Linux development checkout (Nix required).
set -euo pipefail
prim_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
cd "$prim_root"
if [[ ${1:-} == --help ]]; then
    echo 'Usage: ./build.sh [--refresh-deps]  (Linux x86-64; Nix required; JOBS defaults to 4)'
    echo 'Then ./run.sh for VR, ./run.sh --desktop, or ./run.sh --editor.'
    exit 0
fi
refresh_deps=0
if [[ ${1:-} == --refresh-deps ]]; then refresh_deps=1; shift; fi
if (( $# )); then echo 'Unexpected arguments; see ./build.sh --help' >&2; exit 2; fi
if [[ $(uname -sm) != 'Linux x86_64' ]]; then echo 'This helper supports Linux x86-64.' >&2; exit 1; fi
if [[ ${PRIM_LOCAL_DEV_SHELL:-} != 1 ]]; then
    extra=()
    if (( refresh_deps )); then extra+=(--refresh-deps); fi
    exec nix develop "$prim_root" --command env PRIM_LOCAL_DEV_SHELL=1 bash "$prim_root/build.sh" "${extra[@]}"
fi
# Only initialize absent dependencies; never reset existing working trees.
for dependency in godot-libmpv-zero godot-network-audio; do
    if [[ ! -f dependencies/$dependency/README.md ]]; then
        git submodule update --init --recursive -- "dependencies/$dependency"
    fi
done
if [[ ! -f dependencies/godot-libmpv-zero/godot-cpp/CMakeLists.txt ]]; then
    git -C dependencies/godot-libmpv-zero submodule update --init -- godot-cpp
fi
mkdir -p .local project/bin/linux project/addons/godot-steam-audio
for item in godot-interop mpv mpv.dev steam-audio; do
    case "$item" in godot-interop) link=godot;; mpv.dev) link=mpv-dev;; *) link=$item;; esac
    if (( refresh_deps )) || [[ ! -e .local/$link ]]; then
        nix build "./dependencies/godot-libmpv-zero#$item" -o ".local/$link"
    else
        echo "Using staged $link (.local/$link); --refresh-deps rebuilds pinned dependencies."
    fi
done
cargo build --locked --target-dir "$prim_root/target"
cmake -S dependencies/godot-libmpv-zero -B .local/build-linux/video -G Ninja \
    -DMPV_DIR="$prim_root/.local/mpv-dev" -DCMAKE_BUILD_TYPE=Release \
    -DGODOTCPP_BUILD_PROFILE="$prim_root/dependencies/godot-libmpv-zero/build_profile.json" \
    -DGODOTCPP_TARGET=template_debug
cmake --build .local/build-linux/video --parallel "${JOBS:-4}"
cp .local/build-linux/video/bin/*.so project/bin/linux/
cp dependencies/godot-libmpv-zero/project/bin/libmpv_zero.gdextension project/bin/
# Nix outputs are read-only; staged copies must be writable for rebuild/import.
chmod -R u+w project/addons/godot-steam-audio
cp -r .local/steam-audio/bin .local/steam-audio/icons project/addons/godot-steam-audio/
chmod -R u+w project/addons/godot-steam-audio
cp target/debug/libprim_native.so project/bin/linux/
cat > project/bin/prim.gdextension <<'EXT'
[configuration]
entry_symbol = "gdext_rust_init"
compatibility_minimum = "4.7"
reloadable = false
[libraries]
linux.x86_64 = "./linux/libprim_native.so"
windows.x86_64 = "./windows/prim_native.dll"
EXT
python3 tools/fetch-viseme-runtime.py --platform linux
python3 tools/fetch-media-tools.py
mkdir -p .local/dev-tools
ln -sfn ../media-tools/yt-dlp_linux .local/dev-tools/yt-dlp
ln -sfn ../media-tools/linux/deno .local/dev-tools/deno
python3 - <<'PY'
import json, os, secrets
try:
    fd = os.open('project/private_lobby.json', os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
except FileExistsError:
    pass
else:
    with os.fdopen(fd, 'w') as file:
        json.dump({'secret': secrets.token_hex(32)}, file)
    print('Created a local lobby secret; use the same private file for peers that should discover each other.')
PY
LIBMPV_ZERO_MPV_LIBRARY="$prim_root/.local/mpv/lib/libmpv.so.2" \
    .local/godot/bin/godot --headless --path project --xr-mode off --editor --import
echo 'Build ready. Run ./run.sh (VR), ./run.sh --desktop, or ./run.sh --editor.'
