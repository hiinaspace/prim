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
  runtime = pkgs.buildFHSEnv {
    name = "prim-launcher-fhs";
    targetPkgs = p: with p; [
      glibc stdenv.cc.cc openssl icu zlib fontconfig freetype
      libx11 libice libsm libXrandr libxi libXcursor libGL
      fuse fuse3 squashfsTools coreutils bash xdg-utils
    ];
    # FUSE mounting is unavailable inside this user-namespace FHS sandbox.
    profile = ''export APPIMAGE_EXTRACT_AND_RUN=1'';
    runScript = "bash";
  };
in pkgs.mkShell {
  passthru = { inherit runtime; };
  packages = [ (pkgs.dotnetCorePackages.combinePackages [ pkgs.dotnet-sdk_10 pkgs.dotnet-runtime_8 ]) pkgs.squashfsTools pkgs.openssl
    pkgs.python3 pkgs.binutils pkgs.xorg-server pkgs.xwininfo runtime ];
  DOTNET_CLI_TELEMETRY_OPTOUT = "1";
  DOTNET_NOLOGO = "1";
  shellHook = ''export PRIM_LAUNCHER_FHS="${runtime}/bin/prim-launcher-fhs"'';
}
