# Prim's media interop engine plus opt-in, restartable OpenXR sessions.
{ pkgs }:
pkgs.godot_4.overrideAttrs (old: {
  patches = (old.patches or [ ]) ++ [
    ../../dependencies/godot-libmpv-zero/patches/godot/0001-enable-libplacebo-device-features.patch
    ../../dependencies/godot-libmpv-zero/patches/godot/0002-retire-audio-before-extension-unload.patch
    ../../dependencies/godot-libmpv-zero/patches/godot/0003-track-external-texture-writes.patch
    ./0004-restartable-openxr.patch
    ./0005-restore-stereo-on-xr-reentry.patch
    ./0006-companion-overlay.patch
  ];
  sconsFlags = builtins.filter (flag: flag != "debug_symbols=true") old.sconsFlags
    ++ [ "use_static_cpp=false" "debug_symbols=false" ];
})
