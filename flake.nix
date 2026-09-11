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
  in {
    devShells.x86_64-linux.default = pkgs.mkShell {
      packages = with pkgs; [ cargo rustWithTools rustfmt clippy pkg-config cmake ninja
        python3 scons clang lld patchelf binutils ffmpeg libpulseaudio ];
      buildInputs = with pkgs; [ opus openssl alsa-lib ];
      LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
      LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [ pkgs.stdenv.cc.cc.lib pkgs.opus ];
    };
  };
}
