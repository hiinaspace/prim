# Private runtime for the portable game, including host-loaded OpenXR drivers.
{ }:
let
  lock = builtins.fromJSON (builtins.readFile ../flake.lock);
  source = builtins.fetchTree {
    type = "github";
    owner = "NixOS";
    repo = "nixpkgs";
    inherit (lock.nodes.nixpkgs.locked) rev narHash;
  };
  pkgs = import source { system = "x86_64-linux"; };
in pkgs.glibc.overrideAttrs (old: {
  version = "2.43";
  src = pkgs.fetchurl {
    url = "https://ftp.gnu.org/gnu/glibc/glibc-2.43.tar.xz";
    hash = "sha256-2chsa12920Oj4IJwxYRPxRd9GUQs9bjfS+fAfNX6ODE=";
  };
  configureFlags = (old.configureFlags or []) ++ [ "--disable-werror" ];
  patches = builtins.filter
    (patch: !(pkgs.lib.hasSuffix "-master.patch" (toString patch))
      && !(pkgs.lib.hasInfix "Revert-Remove-all-usage-of-BASH" (toString patch)))
    old.patches;
})
