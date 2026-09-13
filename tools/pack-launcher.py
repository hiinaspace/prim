#!/usr/bin/env python3
"""Build a self-contained launcher + prim Velopack package and authenticated feed.

Run in nix-shell launcher/shell.nix on NixOS. Inputs must already be staged game
packages. Output is local; publication is a separate explicit operation.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import shutil
import shlex
import subprocess
import tempfile
import time
import zipfile

ROOT = Path(__file__).resolve().parents[1]
VPK_VERSION = "1.2.0"


def call(*args, **kwargs):
    subprocess.run([str(x) for x in args], check=True, **kwargs)


def check_portable_launcher(stage):
    """Reject builder-specific ELF dependencies before signing a Linux release.

    The game has its own bundled-loader packaging contract, so its ELF
    interpreter is unused; its dynamic dependencies must still be portable.
    """
    for f in stage.rglob("*"):
        if not f.is_file():
            continue
        with f.open("rb") as stream:
            if stream.read(4) != b"\x7fELF":
                continue
        flags = ["-d"] if f.relative_to(stage).parts[0] == "game" else ["-l", "-d"]
        headers = subprocess.check_output(["readelf", *flags, str(f)], text=True)
        if "/nix/store/" in headers:
            raise RuntimeError(f"Nonportable packaged ELF dependency: {f.relative_to(stage)}")


def signed_feed(feed, channel, key, directory):
    contents = {}
    for asset in feed["Assets"]:
        if asset["Type"] != "Full":
            continue
        entries = {}
        with zipfile.ZipFile(directory / asset["FileName"]) as z:
            for entry in z.infolist():
                if entry.is_dir():
                    continue
                with z.open(entry) as f:
                    digest = hashlib.file_digest(f, "sha256").hexdigest()
                entries[entry.filename] = {"Size": entry.file_size, "Sha256": digest, "Executable": False}
        contents[asset["FileName"]] = entries
    payload = json.dumps({"Schema": 1, "Channel": channel, "Sequence": time.time_ns(), "Feed": feed, "PackageContents": contents}, separators=(",", ":")).encode()
    sig = subprocess.check_output(["openssl", "dgst", "-sha256", "-sign", str(key), "-sigopt", "rsa_padding_mode:pss", "-sigopt", "rsa_pss_saltlen:digest"], input=payload)
    return {"Payload": base64.b64encode(payload).decode(), "Signature": base64.b64encode(sig).decode()}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--platform", choices=["linux", "windows"], required=True)
    p.add_argument("--version", required=True)
    p.add_argument("--game-dir", type=Path, required=True)
    p.add_argument("--feed-url", required=True)
    p.add_argument("--key", type=Path, required=True, help="Existing RSA PEM private signing key; never copied to package")
    p.add_argument("--notes", type=Path, required=True)
    p.add_argument("--output", type=Path, default=ROOT / ".local/launcher-releases")
    p.add_argument("--test-loopback", action="store_true", help="Allow localhost HTTP in this test build")
    p.add_argument("--fhs", action="store_true", help="Run vpk/helper binaries through prim-launcher-fhs on NixOS")
    a = p.parse_args()
    if not a.game_dir.is_dir() or not a.key.is_file():
        p.error("Game directory and existing signing key are required")
    a.output = a.output.resolve()
    channel = "friends-" + a.platform
    output = a.output / a.platform
    output.mkdir(parents=True, exist_ok=True)
    if list(output.glob(f"Prim-{a.version}-*-full.nupkg")) or list(output.glob(f"Prim-{a.version}-full.nupkg")):
        p.error("Release version already exists; choose a new immutable version")
    tools = ROOT / ".local/launcher-tools"
    dll = tools / f".store/vpk/{VPK_VERSION}/vpk/{VPK_VERSION}/tools/net8.0/any/vpk.dll"
    if not dll.exists():
        call("dotnet", "tool", "install", "vpk", "--version", VPK_VERSION, "--tool-path", tools)
    rid = "win-x64" if a.platform == "windows" else "linux-x64"
    public = subprocess.check_output(["openssl", "pkey", "-in", str(a.key), "-pubout"], text=True)
    scratch = ROOT / ".local/launcher-staging"
    scratch.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=rid + "-", dir=scratch) as tmp:
        stage = Path(tmp)
        call("dotnet", "publish", ROOT / "launcher/Prim.Launcher", "-c", "Release", "-r", rid,
             "--self-contained", "true", "-p:Version=" + a.version, "-o", stage)
        shutil.copytree(a.game_dir, stage / "game")
        if a.platform == "linux":
            check_portable_launcher(stage)
        (stage / "update-config.json").write_text(json.dumps({"Url": a.feed_url.rstrip("/") + "/" + a.platform + "/", "PublicKey": public, "Channel": channel, "AllowLoopbackHttp": a.test_loopback}, indent=2))
        shutil.copy2(a.notes, stage / "CHANGELOG.md")
        manifest = {}
        for f in sorted(stage.rglob("*")):
            if f.is_symlink():
                raise RuntimeError(f"Stage regular files, not symlinks: {f.relative_to(stage)}")
            if f.is_file():
                # vpk excludes these developer-only files by default.
                if f.suffix == ".pdb" or f.name == "createdump":
                    f.unlink()
                    continue
                with f.open("rb") as stream:
                    digest = hashlib.file_digest(stream, "sha256").hexdigest()
                manifest[f.relative_to(stage).as_posix()] = {"Size": f.stat().st_size, "Sha256": digest, "Executable": bool(f.stat().st_mode & 0o100)}
        (stage / "content-manifest.json").write_text(json.dumps(manifest, indent=2))
        command = ["dotnet", dll, "[win]" if a.platform == "windows" else "[linux]", "pack", "--packId", "Prim", "--packTitle", "prim", "--packVersion", a.version, "--packDir", stage, "--mainExe", "PrimLauncher.exe" if a.platform == "windows" else "PrimLauncher", "--channel", channel, "--runtime", rid, "--releaseNotes", a.notes.resolve(), "--outputDir", output, "--skip-updates", "--yes"]
        if a.fhs:
            command = ["prim-launcher-fhs", "-c", shlex.join([str(x) for x in command])]
        call(*command)
    feed_path = output / f"releases.{channel}.json"
    feed = json.loads(feed_path.read_text())
    signed = output / f"releases.{channel}.signed.json"
    temporary = signed.with_suffix(".tmp")
    temporary.write_text(json.dumps(signed_feed(feed, channel, a.key, output)))
    temporary.replace(signed)
    print("Local signed feed:", signed)


if __name__ == "__main__":
    main()
