#!/usr/bin/env bash
# Build the exact engine patch set used by Prim; Nix reuses unchanged outputs.
set -euo pipefail
prim_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$prim_root"
mkdir -p .local
nix build --impure --expr '
  let
    dependency = builtins.getFlake (toString ./dependencies/godot-libmpv-zero);
    pkgs = import dependency.inputs.nixpkgs { system = "x86_64-linux"; };
  in import ./build-support/godot { inherit pkgs; }
' --cores "${JOBS:-4}" -o .local/godot "$@"
