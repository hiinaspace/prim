#!/usr/bin/env bash
set -euo pipefail
prim_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$prim_root"
platform=${1:-linux}
case "$platform" in linux|windows) ;; *) echo 'Usage: build-openvr-helper.sh [linux|windows]' >&2; exit 2;; esac
mkdir -p .local project/bin/"$platform"
nix build --impure --expr "
  let dependency = builtins.getFlake (toString ./dependencies/godot-libmpv-zero);
      pkgs = import dependency.inputs.nixpkgs { system = \"x86_64-linux\"; };
  in import ./build-support/openvr-helper.nix { inherit pkgs; windows = $([[ $platform == windows ]] && echo true || echo false); }
" -o ".local/openvr-helper-$platform"
for source in .local/openvr-helper-"$platform"/bin/*; do
    install -m755 "$source" "project/bin/$platform/$(basename "$source")"
done
