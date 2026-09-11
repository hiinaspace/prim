#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
video_project="${PRIM_VIDEO_PROJECT:-$PWD/dependencies/godot-libmpv-zero/project}"
mkdir -p project/bin/linux project/addons
cp "$video_project/bin/libmpv_zero.gdextension" project/bin/
cp "$video_project/bin/linux/"*.so project/bin/linux/
cp -a "$video_project/addons/godot-steam-audio" project/addons/
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
cp dependencies/godot-libmpv-zero/project/openxr_action_map.tres project/
