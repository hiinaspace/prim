#!/usr/bin/env bash
# Disposable Linux runtime-ownership diagnostic, using the pinned SDK/loader.
set -euo pipefail
prim_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$prim_root"
mkdir -p .local
nix build --impure --expr '
  let
    dependency = builtins.getFlake (toString ./dependencies/godot-libmpv-zero);
    pkgs = import dependency.inputs.nixpkgs { system = "x86_64-linux"; };
  in pkgs.stdenv.mkDerivation {
    name = "prim-steamvr-probe";
    src = ./tools/steamvr;
    buildInputs = [ pkgs.openvr pkgs.openxr-loader ];
    buildPhase = "$CXX -std=c++17 probe.cpp -lopenvr_api -lopenxr_loader -pthread -o prim-steamvr-probe";
    installPhase = "mkdir -p $out/bin; cp prim-steamvr-probe $out/bin/";
  }
' -o .local/steamvr-probe "$@"
