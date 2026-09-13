# Friends deployment

Published 2026-09-13: https://prim.hiina.space/ — 0.3.5 for both launcher channels, combining host-file
streaming, voice-driven visemes, and the existing Linux portability fixes.
Game inputs: `dist/prim-0.3.5-{linux,windows}`. Signed package output:
`.local/launcher-production-035/{linux,windows}` in the streaming worktree.
0.3.4 drafts were never published; keep them outside production feeds.
The verified production output is also retained under the main checkout's
`.local/launcher-production`, so later releases can generate deltas from 0.3.5.

Source: Prim `0022fcf` on GitHub main, including merged visemes `31ff698`;
public GNA `d9b61bc`. Previous bootstraps/feeds are backed up on chirashi at
`/mnt/nvme/prim/.backup-before-0.3.5`. Upload hashes were verified before feeds
were replaced. Public signature checks, range/cache headers, bootstrap checksums
and a full Windows-package download hash passed. An isolated Ubuntu launcher
updated from 0.3.3 to 0.3.5 through the public feed, restarted and verified its
files with `/nix` hidden. That run used the full package because the isolated
installation had no cached base; delta packages are available but their
application was not retested for this release.


The checks for this release are in `docs/TESTING.md`. Everyone must update because
the room hello is now version 3. Packaging Windows requires `--runtime-directory`
pointing to the MinGW mcfgthread runtime; missing compiler DLLs now fail packaging.

Previous live deployment: Linux 0.3.3, Windows 0.3.2; same VRM game/protocol.

Linux 0.3.3 fixes the Nix-specific .NET runtime/apphost packs, libmpv's absolute
mujs dependency, and host graphics-library discovery. Its game input is
`dist/prim-avatar-portable-linux`, a copy of 0.3.2 with only the wrapper and libmpv
dependency reference repaired. The reusable game packaging fix is in
`tools/package-linux.py`; avatars/PCK/native source revisions are unchanged.
Pre-release drafts remain in `.local/launcher-drafts`, not served. The previous
Linux bootstrap/feed/page is backed up on chirashi at
`/mnt/nvme/prim/.backup-linux-0.3.2-before-0.3.3`.

Validated on natto (Ubuntu 26.04.1) with `/nix` hidden using bubblewrap: CLI,
23 self-checks, file verification, GUI smoke, and 90-frame desktop game smoke with
Vulkan/llvmpipe, Steam Audio and libmpv initialization; exit 0. The same game smoke
passed on NixOS. The old 0.3.2 launcher fails the isolated startup test. This does
not validate natto headset behavior or hardware video decoding.

The actual 0.3.1 → 0.3.2 VRM delta applied on natto with no full fallback and
restarted into 0.3.2. Linux: 38.11 MiB delta vs 373.71 MiB full. Windows package
sizes: 29.33 MiB delta vs 283.32 MiB full (~90% savings). A subsequent update through
the public feed also applied the 0.3.2 → 0.3.3 fix and restarted successfully.

Release 0.3.2 packages use `dist/prim-avatar-linux` and `dist/prim-avatar-windows`.
Avatar source commit: `0f6345c` on `codex/prim-mvp`. Both signed feeds retain
0.3.1 full packages and provide 0.3.2 full/delta packages. Uploaded bytes were
SHA256-verified before publication; feeds were replaced after packages. Previous
feeds/bootstraps are retained in `/mnt/nvme/prim/.backup-0.3.1-before-0.3.2`.
The user tested single-player headset use successfully; scaled-foot log spam
and preview mirroring fixes have automated regression coverage.

- Host: `chirashi`; root: `/mnt/nvme/prim` (separate from the miscellaneous file site).
- nginx: `/etc/nginx/sites-available/prim.hiina.space`, symlink in `sites-enabled`.
  The local source is `prim.hiina.space.conf` beside this document.
- TLS: existing `/etc/letsencrypt/live/hiina.space/` wildcard certificate.
- DNS already resolved to chirashi's public address; no DNS change or Cloudflare
  reverse proxy was needed.
- Feed base: `https://prim.hiina.space/downloads`; channels `friends-linux` and
  `friends-windows`. Signed manifests use no-store; versioned nupkgs are immutable.
- Local production packages: `.local/launcher-production/{linux,windows}`.
- **Persistent release signing key:**
  `/home/s/.local/share/prim-release-signing/prim-release.pem` (private, outside the
  build cache). Keep and back up this key; do not generate a new one per release.
  It was not uploaded to the webserver. This is update-feed signing, not Authenticode.

## Next release

Build sequentially from the repository root inside `nix-shell launcher/shell.nix`,
using a fresh version number and the appropriate staged game folder:

```sh
python3 tools/pack-launcher.py \
  --platform linux --version 0.3.4 \
  --game-dir dist/prim-avatar-portable-linux \
  --feed-url https://prim.hiina.space/downloads \
  --key /home/s/.local/share/prim-release-signing/prim-release.pem \
  --notes launcher/releases/0.3.4.md \
  --output .local/launcher-production --fhs
```

Repeat for Windows with its staged game directory. Retain the previous production
full nupkgs locally so vpk can create deltas. Never mix the localhost evaluation
output into this production directory or rebuild the same release version.

Upload new immutable nupkgs to a hidden staging directory on chirashi, verify SHA256
against the local build, then rename them into `downloads/linux` or
`downloads/windows`. Replace bootstrap files from staging with same-filesystem
renames. Replace each signed manifest **last**, after every referenced package is
available. Serialize publication; retain old packages referenced by the feed.
Update `index.html` and public `SHA256SUMS` to match. nginx does not need a reload
for a content-only release.

For configuration changes, install this vhost file, run `sudo -n nginx -t`, then
`sudo -n systemctl reload nginx`. Do not edit other vhosts. Existing protocol-option
warnings for funkwhale/ntfy were present before this deployment and unchanged.

## Verified after deployment

- HTTPS certificate validation, HTTP redirect, page bytes, all four download links,
  byte-range responses, and manifest/package cache headers.
- Every uploaded file matched its local SHA256; the complete public Windows
  installer was also fetched and hashed independently.
- Linux production launcher authenticated the live feed, downloaded and applied a
  full repair over HTTPS, then restarted into 0.3.1.
- Hosted Nix expression evaluates to the same tested FHS runtime store path.
- Hidden staging and directory browsing are unavailable. The site has no login;
  robots/noindex headers discourage indexing but do not provide access control.

The older localhost launchers need a one-time download from this site. They cannot
discover the new origin themselves. Friend testing of native Windows installation
and both VR paths remains useful; this deployment adds no new claim about those.

Host driver SONAME discovery follows the [Khronos loader documentation](https://github.com/KhronosGroup/Vulkan-Loader/blob/main/docs/LoaderDriverInterface.md); the wrapper keeps bundled libraries first and adds host library directories for drivers.
