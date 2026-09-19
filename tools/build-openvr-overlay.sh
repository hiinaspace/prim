#!/usr/bin/env bash
set -euo pipefail
prim_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$prim_root"
platform=${1:-linux}
case $platform in linux|windows) ;; *) echo 'Usage: build-openvr-overlay.sh [linux|windows]' >&2; exit 2;; esac
revision=c766e97e5173276b3d90bc3b38f8bd76a2de00d7
patch_id=$(sha256sum build-support/openvr-overlay/0001-prim-lifecycle.patch | cut -c1-16)
source_dir="$prim_root/.local/openvr-overlay/$revision-$patch_id"
if [[ ! -f $source_dir/.prim-patched ]]; then
    if [[ -d $source_dir ]]; then echo "Incomplete source cache: $source_dir; inspect/remove it before retrying." >&2; exit 1; fi
    mkdir -p "$(dirname "$source_dir")"
    git clone https://github.com/hiinaspace/godot-openvr-overlay.git "$source_dir"
    git -C "$source_dir" checkout --detach "$revision"
    git -C "$source_dir" submodule update --init --recursive
    git -C "$source_dir" apply "$prim_root/build-support/openvr-overlay/0001-prim-lifecycle.patch"
    touch "$source_dir/.prim-patched"
fi
sdk=$(nix eval --impure --raw --expr 'let d = builtins.getFlake (toString ./dependencies/godot-libmpv-zero); p = import d.inputs.nixpkgs { system="x86_64-linux"; }; in toString p.openvr.src')
extra=()
build_command=(scons)
if [[ $platform == windows ]]; then
    # Use the same cross toolchain prepared for the Windows engine build.
    mingw=${MINGW_PREFIX:-$prim_root/.local/windows/scons-toolchain}
    [[ -x $mingw/bin/x86_64-w64-mingw32-g++ ]] || { echo 'Prepare the Windows cross toolchain (docs/BUILDING.md) or set MINGW_PREFIX.' >&2; exit 1; }
    extra+=("mingw_prefix=$mingw" use_mingw=yes use_static_cpp=yes)
    build_command=(bash -c 'export PATH="$1/bin:$PATH"; shift; exec scons "$@"' _ "$mingw")
fi
nix develop --command "${build_command[@]}" -C "$source_dir" platform="$platform" target=template_debug \
    OPENVR_DIR="$sdk" build_profile="$prim_root/build-support/openvr-overlay/build_profile.json" "${extra[@]}" -j"${PRIM_BUILD_JOBS:-4}"
mkdir -p "project/bin/$platform"
stage_binary() {
    local source=$1 destination="project/bin/$platform/$(basename "$1")"
    install -m755 "$source" "$destination.new"
    mv -f "$destination.new" "$destination"
}
if [[ $platform == linux ]]; then
    stage_binary "$source_dir/bin/linux/libgodot-openvr-overlay.linux.template_debug.x86_64.so"
else
    stage_binary "$source_dir/bin/windows/libgodot-openvr-overlay.windows.template_debug.x86_64.dll"
    stage_binary "$sdk/bin/win64/openvr_api.dll"
fi
