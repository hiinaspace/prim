#!/usr/bin/env bash
# Resolve the exact plugin patch set, including local uncommitted patches.
set -euo pipefail
prim_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$prim_root"
mkdir -p .local
nix build --impure --expr '
  let
    dependency = builtins.getFlake (toString ./dependencies/godot-libmpv-zero);
    pkgs = import dependency.inputs.nixpkgs { system = "x86_64-linux"; };
  in import ./dependencies/godot-libmpv-zero/nix/steam-audio.nix { inherit pkgs; }
' --cores "${JOBS:-4}" -o .local/steam-audio "$@"
