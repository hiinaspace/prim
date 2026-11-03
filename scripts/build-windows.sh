#!/usr/bin/env bash
# Build an unpublished Windows x86-64 test zip from this checkout (Linux + Nix).
#
# What this rebuilds: the Rust extension (prim_native.dll) and the game PCK.
# What it reuses: the patched Godot engine, libmpv, Steam Audio, libmpv-zero,
# ONNX Runtime and the other DLLs from an already-built Windows bundle (--base),
# e.g. tools/package-windows.py output or an older Prim Windows release. Engine and
# extension sources are pinned by build-support/ and dependencies/; check
# `git diff <base-commit> HEAD -- build-support dependencies` is empty before
# trusting a base. The full engine/mpv cross-build recipe is in docs/BUILDING.md.
#
# Usage: scripts/build-windows.sh --base DIR [--out DIR] [--name NAME]
# Output: <out>/<name>.zip (default .local/dist/prim-windows-test.zip). Never published.
set -euo pipefail
prim_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$prim_root"

base=${PRIM_WINDOWS_BASE:-}
out=$prim_root/.local/dist
name=prim-windows-test
while (( $# )); do
    case "$1" in
        --base) base=$2; shift 2;;
        --out) out=$2; shift 2;;
        --name) name=$2; shift 2;;
        --help) sed -n 2,13p "$0"; exit 0;;
        *) echo "Unexpected argument $1" >&2; exit 2;;
    esac
done
[[ -f $base/prim.exe && -f $base/libmpv-2.dll ]] || { echo 'Need --base DIR (a prebuilt Windows bundle with prim.exe and libmpv-2.dll).' >&2; exit 2; }

if [[ ${PRIM_WINDOWS_SHELL:-} != 1 ]]; then
    exec nix develop "$prim_root#windows" --command env PRIM_WINDOWS_SHELL=1 bash "$prim_root/scripts/build-windows.sh" --base "$base" --out "$out" --name "$name"
fi

work=$prim_root/.local/windows
mkdir -p "$work/packages" "$work/opus" "$out"

# 1. Pinned MSYS2 opus (the Rust opus binding links it dynamically).
read -r opus_file opus_sha < <(python3 -I - <<'PY'
import json
for p in json.load(open('build-support/windows-packages.json')):
    if p['name'] == 'mingw-w64-x86_64-opus': print(p['file'], p['sha256'])
PY
)
if [[ ! -f $work/packages/$opus_file ]]; then
    curl -fsSL -o "$work/packages/$opus_file.part" "https://repo.msys2.org/mingw/mingw64/$opus_file"
    mv "$work/packages/$opus_file.part" "$work/packages/$opus_file"
fi
echo "$opus_sha  $work/packages/$opus_file" | sha256sum -c -
[[ -d $work/opus/mingw64 ]] || tar --zstd -xf "$work/packages/$opus_file" -C "$work/opus" --wildcards 'mingw64/*'

# 2. Rust extension.
gcc_bin=$(dirname "$(command -v x86_64-w64-mingw32-gcc)")
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=$gcc_bin/x86_64-w64-mingw32-gcc
export CC_x86_64_pc_windows_gnu=$gcc_bin/x86_64-w64-mingw32-gcc
export AR_x86_64_pc_windows_gnu=$gcc_bin/x86_64-w64-mingw32-ar
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR=$work/opus
export PKG_CONFIG_PATH=$work/opus/mingw64/lib/pkgconfig
export RUSTFLAGS="-L native=$work/opus/mingw64/lib -L native=$WINPTHREADS_LIB"
cargo build --locked --release --target x86_64-pc-windows-gnu --target-dir "$work/rust-build"
dll=$work/rust-build/x86_64-pc-windows-gnu/release/prim_native.dll

# 3. Assemble: base runtime + new extension + libopus + PCK.
stage=$out/$name
rm -rf -- "$stage"
cp -r "$base" "$stage"
rm -f "$stage"/*.pck "$stage"/prim_native.dll
cp "$dll" "$stage/prim_native.dll"
x86_64-w64-mingw32-strip --strip-debug "$stage/prim_native.dll"
cp "$work/opus/mingw64/bin/libopus-0.dll" "$stage/"
[[ -f $stage/bin/windows ]] || mkdir -p "$stage/bin/windows"
[[ -f project/private_lobby.json ]] || python3 - <<'PY'
import json, os, secrets
fd = os.open('project/private_lobby.json', os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
with os.fdopen(fd, 'w') as f: json.dump({'secret': secrets.token_hex(32)}, f)
PY
godot=${GODOT:-$prim_root/.local/godot/bin/godot}
"$godot" --headless --xr-mode off --path project --export-pack Windows "$stage/prim.pck"
if [[ -f scripts/windows-readme.txt ]]; then cp scripts/windows-readme.txt "$stage/README.txt"; fi
for launcher in Play.bat Desktop.bat VR.bat; do
    [[ -f $stage/$launcher ]] || echo "warning: base has no $launcher" >&2
done

# 4. Zip.
rm -f -- "$out/$name.zip"
(cd "$out" && python3 -I -c "
import shutil, sys
shutil.make_archive(sys.argv[1], 'zip', '.', sys.argv[1])" "$name")
sha256sum "$out/$name.zip" | tee "$out/$name.zip.sha256"
echo "Windows test zip: $out/$name.zip (unpublished; do not upload to the update feed)"
