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
  in {
    devShells.x86_64-linux.windows = pkgs.mkShell {
      packages = with pkgs; [ cargo mingw.buildPackages.rustc pkg-config cmake ninja zstd
        (python3.withPackages (ps: [ ps.jinja2 ps.mako ])) mingw.stdenv.cc binutils ];
      WINPTHREADS_LIB = "${mingw.windows.pthreads}/lib";
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
