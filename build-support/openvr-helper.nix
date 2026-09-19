# Same pinned OpenVR SDK for Linux and Windows helpers; no engine dependency.
{ pkgs, windows ? false }:
let
  target = if windows then pkgs.pkgsCross.mingwW64 else pkgs;
  sdk = pkgs.openvr.src;
in target.stdenv.mkDerivation {
  pname = "prim-openvr-helper";
  version = "1";
  src = ../native/openvr-helper;
  buildPhase = if windows then ''
    $CXX -std=c++17 -O2 -I${sdk}/headers main.cpp \
      ${sdk}/lib/win64/openvr_api.lib -static -o prim-openvr-helper.exe
  '' else ''
    $CXX -std=c++17 -O2 -I${pkgs.openvr}/include/openvr main.cpp \
      -L${pkgs.openvr}/lib -lopenvr_api -o prim-openvr-helper
  '';
  installPhase = ''
    mkdir -p $out/bin $out/share/licenses/openvr
    cp prim-openvr-helper* $out/bin/
    cp ${sdk}/LICENSE $out/share/licenses/openvr/LICENSE
  '' + pkgs.lib.optionalString windows ''
    cp ${sdk}/bin/win64/openvr_api.dll $out/bin/
  '';
}
