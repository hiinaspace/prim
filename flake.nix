{
  description = "Native development tools for the prim theater prototype";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/dc5d91f840324650bac8c379428c7037a416959a";
  outputs = { self, nixpkgs }: let
    pkgs = import nixpkgs { system = "x86_64-linux"; };
    rustWithTools = pkgs.runCommand "prim-rustc-with-llvm-tools" { nativeBuildInputs = [ pkgs.makeWrapper ]; } ''
      mkdir -p "$out"
      cp -rs ${pkgs.rustc.unwrapped}/. "$out/"
      chmod u+w "$out/bin" "$out/lib/rustlib/x86_64-unknown-linux-gnu/bin"
      rm "$out/bin/rustc"
      makeWrapper ${pkgs.rustc}/bin/rustc "$out/bin/rustc" --add-flags "--sysroot $out"
      ln -s ${pkgs.llvmPackages.llvm}/bin/llvm-nm "$out/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-nm"
      ln -s ${pkgs.llvmPackages.llvm}/bin/llvm-objcopy "$out/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-objcopy"
    '';
    # Windows cross shell: nixpkgs' rustc built with the windows-gnu std (std must match the compiler build exactly)
    # and the pinned mingw-w64 gcc as linker.
    mingw = pkgs.pkgsCross.mingwW64;
    # aarch64 cross shell (Steam Frame, SteamOS glibc 2.39): nixpkgs' rustc built with the aarch64 std,
    # the pinned aarch64 gcc as linker. Binaries get the system loader and no Nix rpath so they run on the Frame.
    arm = pkgs.pkgsCross.aarch64-multiplatform;
  in {
    devShells.x86_64-linux.windows = pkgs.mkShell {
      packages = with pkgs; [ cargo mingw.buildPackages.rustc pkg-config cmake ninja zstd
        (python3.withPackages (ps: [ ps.jinja2 ps.mako ])) mingw.stdenv.cc binutils ];
      WINPTHREADS_LIB = "${mingw.windows.pthreads}/lib";
    };
    devShells.x86_64-linux.aarch64 = pkgs.mkShell {
      packages = with pkgs; [ cargo arm.buildPackages.rustc arm.stdenv.cc pkg-config cmake ninja patchelf binutils arm.alsa-lib ];
      # cpal (prim-voice's `device` feature) links libasound: let pkg-config find the aarch64 one.
      PKG_CONFIG_ALLOW_CROSS = "1";
      PKG_CONFIG_PATH_aarch64_unknown_linux_gnu = "${arm.alsa-lib.dev}/lib/pkgconfig";
      CARGO_BUILD_TARGET = "aarch64-unknown-linux-gnu";
      CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER = "aarch64-unknown-linux-gnu-cc";
      CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS = "-C link-arg=-Wl,--dynamic-linker=/lib/ld-linux-aarch64.so.1";
      CC_aarch64_unknown_linux_gnu = "aarch64-unknown-linux-gnu-cc";
      CXX_aarch64_unknown_linux_gnu = "aarch64-unknown-linux-gnu-c++";
      AR_aarch64_unknown_linux_gnu = "aarch64-unknown-linux-gnu-ar";
      # audiopus_sys builds its bundled opus with CMake 4, which needs the old-policy floor; link it static
      # so Frame binaries need no libopus. No Nix store rpath in target binaries.
      CMAKE_POLICY_VERSION_MINIMUM = "3.5";
      LIBOPUS_STATIC = "1";
      NIX_DONT_SET_RPATH_aarch64_unknown_linux_gnu = "1";
      NIX_NO_SELF_RPATH = "1";
    };
    devShells.x86_64-linux.default = pkgs.mkShell {
      packages = with pkgs; [ cargo rustWithTools rustfmt clippy pkg-config cmake ninja
        (python3.withPackages (ps: [ ps.jinja2 ps.mako ])) scons clang lld patchelf binutils ffmpeg libpulseaudio meson zstd nasm vulkan-headers ];
      buildInputs = with pkgs; [ opus openssl alsa-lib ];
      LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
      LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [ pkgs.stdenv.cc.cc.lib pkgs.opus ];
    };
  };
}
