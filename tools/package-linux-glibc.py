#!/usr/bin/env python3
"""Install a coherent glibc runtime into a staged portable Linux game.

Also supports packaging-only hotfixes of an existing release: copy the released
game directory first, then run this tool on the copy. Never mix individual host
libc/libm files with the bundled loader.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def install(game, runtime):
    for tool in ("readelf", "patchelf"):
        if shutil.which(tool) is None:
            raise RuntimeError(f"Required packaging tool not found: {tool}")
    lib = game / "lib"
    if not (lib / "ld-linux-x86-64.so.2").is_file():
        raise RuntimeError("Expected an already staged portable Linux game")
    versions = subprocess.check_output(
        ["readelf", "--version-info", runtime / "lib/libm.so.6"], text=True)
    if "Name: GLIBC_2.43" not in versions:
        raise RuntimeError("OpenXR compatibility requires libm GLIBC_2.43")
    libraries = []
    for source in sorted((runtime / "lib").glob("*.so*")):
        with source.open("rb") as stream:
            if stream.read(4) == b"\x7fELF":
                libraries.append(source)
    for required in ("ld-linux-x86-64.so.2", "libc.so.6", "libm.so.6"):
        if required not in {p.name for p in libraries}:
            raise RuntimeError(f"Incomplete glibc runtime: {required}")
    manifest = {}
    for source in libraries:
        target = lib / source.name
        # Unlink first so a staged symlink cannot overwrite an external file.
        target.unlink(missing_ok=True)
        shutil.copyfile(source, target)
        target.chmod(0o755)
        if not source.name.startswith("ld-linux"):
            subprocess.run(["patchelf", "--set-rpath", "$ORIGIN", target], check=True)
        headers = subprocess.check_output(["readelf", "-d", target], text=True)
        if "/nix/store/" in headers:
            raise RuntimeError(f"Nonportable glibc dependency: {target}")
        manifest[source.name] = hashlib.sha256(target.read_bytes()).hexdigest()
    (game / "glibc-runtime.json").write_text(json.dumps({
        "minimum_libm_version": "GLIBC_2.43", "sha256": manifest,
    }, indent=2) + "\n")
    inventory = game / "runtime-libraries.json"
    if inventory.exists():
        inventory.write_text(json.dumps(sorted(set(json.loads(inventory.read_text()))
                                              | set(manifest)), indent=2) + "\n")
    print(f"Installed {len(manifest)} matched glibc libraries in {game}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--game-dir", type=Path, required=True)
    parser.add_argument("--glibc", type=Path, help="Prebuilt glibc output; defaults to pinned Nix build")
    args = parser.parse_args()
    runtime = args.glibc
    if runtime is None:
        (ROOT / ".local").mkdir(exist_ok=True)
        runtime = Path(subprocess.check_output([
            "nix-build", ROOT / "build-support/linux-glibc.nix",
            "-o", ROOT / ".local/linux-glibc", "--cores", "4",
        ], text=True).strip())
    install(args.game_dir.resolve(), runtime.resolve())
