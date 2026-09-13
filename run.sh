#!/usr/bin/env bash
# Launch staged development files without rebuilding or changing audio devices.
set -euo pipefail
prim_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
cd "$prim_root"
if [[ ${1:-} == --help ]]; then
    echo 'Usage: ./run.sh [--desktop|--editor] [--test-vrm /path/model.vrm] [Godot arguments...]'
    echo 'Default: VR. Build first with ./build.sh. Example smoke: ./run.sh --desktop --headless --quit-after 90'
    exit 0
fi
if [[ ${PRIM_LOCAL_DEV_SHELL:-} != 1 ]]; then
    exec nix develop "$prim_root" --command env PRIM_LOCAL_DEV_SHELL=1 bash "$prim_root/run.sh" "$@"
fi
for file in .local/godot/bin/godot .local/mpv/lib/libmpv.so.2 \
    project/bin/linux/libprim_native.so project/bin/linux/libonnxruntime.so \
    project/bin/linux/libmpv_zero.linux.template_debug.x86_64.so \
    project/addons/godot-steam-audio/bin/libphonon.so project/private_lobby.json \
    .local/dev-tools/yt-dlp .local/dev-tools/deno; do
    if [[ ! -f $file ]]; then echo "Missing $file; run ./build.sh first." >&2; exit 1; fi
done
export LIBMPV_ZERO_MPV_LIBRARY="$prim_root/.local/mpv/lib/libmpv.so.2"
export PATH="$prim_root/.local/dev-tools:$PATH"
# This host exposes X11 without always exporting DISPLAY to terminal agents.
if [[ -z ${DISPLAY:-} && -z ${WAYLAND_DISPLAY:-} && -S /tmp/.X11-unix/X0 ]]; then export DISPLAY=:0; fi
prim_project="$prim_root/project"
forwarded=()
while (( $# )); do
    if [[ $1 == --test-vrm ]]; then
        if (( $# < 2 )); then echo '--test-vrm requires a file path' >&2; exit 2; fi
        prim_project=$(python3 tools/prepare-vrm-preview.py "$2")
        shift 2
    else
        forwarded+=("$1")
        shift
    fi
done
set -- "${forwarded[@]}"
mode=${1:-}
case "$mode" in
    --desktop) shift; exec .local/godot/bin/godot --path "$prim_project" --xr-mode off "$@" -- --desktop;;
    --editor) shift; exec .local/godot/bin/godot --path "$prim_project" --xr-mode off --editor "$@";;
    *) exec .local/godot/bin/godot --path "$prim_project" "$@";;
esac
