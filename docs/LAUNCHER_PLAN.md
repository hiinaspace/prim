# Launcher, updates, and support reports

Implementation decision, 2026-09-12. Adopt **Avalonia + self-contained .NET 10 +
Velopack 1.2.0**. The implementation lives in `launcher/Prim.Launcher`; build and
operating instructions are in `launcher/README.md`. The friends deployment is live
at https://prim.hiina.space/ with production release 0.3.1; see
`launcher/hosting/DEPLOYMENT.md`. Older evaluation artifacts use a localhost feed.

## Decision and Linux dependency evaluation

The existing Quake launcher contains Avalonia assemblies and runs through an FHS
wrapper on this NixOS machine. Its GUI source was not found in the inspected Quake
directories. That establishes useful local compatibility evidence, not proof that
every friend's Linux setup has the same native libraries.

Self-contained .NET publishes include the managed runtime: users do not install
.NET. Native OS dependencies remain: glibc/libstdc++, ICU, OpenSSL, fontconfig and
X11 libraries. This launcher uses software rendering and the X11 backend (XWayland
on Wayland), keeping it independent of prim's Vulkan/OpenXR initialization.
The CLI does not initialize Avalonia. See Microsoft's
[deployment model](https://learn.microsoft.com/en-us/dotnet/core/deploying/) and
[Avalonia's Linux guidance](https://docs.avaloniaui.net/docs/platform-specific-guides/linux).

The included Nix FHS runtime supplies those native dependencies. It runs the
AppImage with `APPIMAGE_EXTRACT_AND_RUN=1`, because FUSE mounting failed inside
this FHS user namespace. Extraction costs startup time and temporary disk space;
it worked in the actual package test. An ordinary Linux desktop may use FUSE
instead, but that path has not been tested here. Alpine/musl and ARM are outside
the current x64/glibc target. Self-contained .NET does not mean universal Linux
compatibility.

The FHS runtime is separately buildable without a development SDK in its closure.
The Nix install wrapper retains its store path; keep its GC root. Update staging,
helper executables, and helper working directories persist outside nix-shell's
short-lived temporary directory. The helper is copied with its required
`usr/bin/UpdateNix` and `sq.version` AppDir layout, while the installed AppImage
remains Velopack's update target.

## Why accept Velopack's packaging layout

Velopack supplies release selection, package/delta creation, the native patcher,
full-package fallback, process-exit waiting, platform application replacement and
restart. Windows receives Setup.exe/portable ZIP; Linux receives an AppImage.
The launcher and staged `game/` travel together, so a launcher self-update uses the
same transaction. Settings and logs remain outside that package.

This replaces the earlier proposal for individually compressed files and a custom
release-directory switcher. The binary delta is now an upstream responsibility.
An observed test release reduced a roughly 351 MiB full Linux package to about
212 KiB of delta. This is one small-change sample, not a promise for native-library
changes. Linux's first update normally downloads a full nupkg because the initial
AppImage has no cached base package; subsequent updates can use deltas. Windows
Setup seeds its package cache. A missing/untrusted base falls back to full.

Source review pinned to [Velopack 1.2.0](https://github.com/velopack/velopack/tree/1.2.0)
(commit `f2edcbcafb81da5b3c884aaea330e225ad91d8b6`), especially
`src/lib-csharp/UpdateManager.cs`, `src/lib-rust/src/locator.rs` and
`src/bins/src/commands/apply_linux_impl.rs`. Also see
[delta packaging](https://docs.velopack.io/packaging/deltas).

Prim-specific adapters are deliberately small:

- A signed update source, independent of Velopack's normal unsigned JSON feed.
- Verification of cached packages before use and again before apply.
- Linux package/scratch directories beside the AppImage, avoiding a cross-filesystem
  copy when Velopack replaces it, plus a persistent detached helper.
- A protected delta-method override that passes `--packageDir` and supports
  cancellation. It still invokes Velopack's native patch engine and keeps the
  manager's full-download fallback. Recheck these adapters on a Velopack upgrade.

## Implemented behavior

- Windows/Linux x64 packaging; desktop and VR launch buttons.
- Check on startup, show changelog; download and apply only when clicked.
- Keep playing while a download is prepared. Installation refuses while the
  launcher-owned game is running. A PID plus start-time record detects a surviving
  game after a launcher crash. An independently launched game is not tracked.
- Per-user state, Linux default/custom install buttons and user shortcut;
  Windows standard per-user Setup with `--installto` for a custom path.
- Verify packaged files, download repair for the installed version, or prepare
  the previous version still present in the feed. Repair/rollback requires a
  separate Install and restart click. A launcher too broken to start needs the
  bootstrap download again. This is not automatic crash-triggered rollback.
- Capture each game session's stdout/stderr, Godot log, build version, mode and
  exit status. Keep the launcher open while playing.
- Create a local support ZIP and open its folder. No upload, telemetry SDK or
  account system.

The previous `dist/feedback-4` exports are packaging inputs in the evaluation;
`build-revisions.json` identifies prim `c7abe848307d7a4fd1b4df3e88e56cd10a285498`.
These are not claims that this launcher work rebuilds the current game checkout.

## Update authenticity and publication

Velopack's normal package hashes detect damage; in the inspected SDK they do not
by themselves authenticate the feed. Prim embeds an RSA public key and verifies
an RSA-PSS/SHA-256 signature over the exact bytes of a base64 payload containing
channel, sequence, asset filenames/sizes/hashes and package entry hashes. The
private signing key stays on the publisher. Production HTTPS is mandatory;
loopback HTTP requires an explicit build flag. Redirects are disabled.

Downloaded full/delta packages must match signed SHA-256 and length. Velopack
reconstructs a ZIP with different compression bytes after a delta: its whole-file
hash can differ even when every entry is identical. Therefore signed metadata also
contains the full package's entry names, lengths and SHA-256 values. Reconstructed
packages must match that exact entry set; extra, missing, duplicate or changed
entries are rejected. Cached packages receive the same check before reuse.

A persisted increasing feed sequence rejects metadata older than one that client
has seen. This is a small private-project protocol, not TUF: no expiry policy,
delegated roles, threshold signatures or automatic key rotation. HTTPS and the
private-link approach remain part of the distribution model. An unlisted URL is
not authentication, and the existing friends' lobby configuration remains inside
the distributed game. Do not log feed path credentials or include them in reports.

This signature is separate from Windows Authenticode/reputation. The generated
Setup/application binaries are not OS-signed; this implementation does not remove
Windows unknown-publisher/SmartScreen warnings. See
[Velopack signing](https://docs.velopack.io/packaging/signing).

Use an unlisted prefix under `/mnt/www` initially. Upload immutable versioned
nupkgs first, verify their remote bytes, then atomically replace the signed feed
last on the server filesystem. Publish bootstrap AppImage/Setup downloads too.
Keep older full packages referenced by the feed for repair/previous-version use.
Serialize publication for a channel and never rebuild a published version number.
The packaging script writes its local signed feed last but is not a concurrent
publisher or remote deployment tool.

Recommended nginx cache policy: no-store on `releases.*.signed.json`; long immutable
caching on versioned nupkgs; revalidate unversioned bootstrap filenames. The client
sends no-cache for feed fetches. A stale cache may delay discovery on a new client;
existing clients reject a lower sequence. If Cloudflare becomes inconvenient,
use the proposed direct `prim.hiina.space` vhost with the wildcard certificate.
The feed origin is bundled, so plan a launcher release while the old origin still
works when moving it. No server configuration changes are required for evaluation.

## Local diagnostics

Reports contain selected OS/CPU/runtime metadata, configured OpenXR manifest and
runtime name/library, launcher/updater logs, the last three launcher-owned sessions,
build revisions and last verification result. Actual GPU/XR initialization comes
from the app logs; finding a configured runtime does not prove VR works. Each
included text file is capped at 2 MiB and the source-text total at 20 MiB. Reports
list included files and truncation. Stdout/stderr capture is capped while pipes
continue draining so a verbose child cannot deadlock.

Export starts from an allowlist, then scrubs home/install paths, media-source log
events, URL paths/query/credentials, common secret fields, long IDs, email and IP
addresses. It preserves timestamps. User settings, environment dumps, media,
screenshots, arbitrary files, full crash/core dumps and resume secrets are excluded.
This is basic friend-context scrubbing, not a guarantee that arbitrary application
text contains no sensitive data. Users review the ZIP before sharing. No network
secret-verification scanners run as part of export.

Godot/stderr backtrace text is included when present. WER, coredumpctl, Crashpad and
symbol-server integration are deferred; retain matching build symbols separately
if native crash diagnosis becomes useful. Existing app logs are the MVP's primary
evidence. Logs/reports are local and currently need manual cleanup; automatic
session retention and a short bounded performance-recording mode are future work.

## Validation and remaining gates

See `launcher/README.md` for repeatable commands and the current evidence. Linux
packaging/runtime tests run on NixOS through the FHS wrapper. These do not establish
headset rendering, controller input, voice or media playback. Windows cross-build
success does not establish Windows execution. Further friend testing should exercise native Windows install/update/repair and
desktop/VR launch on both targets,
plus one ordinary glibc Linux system. Package with the actual private HTTPS feed
and a backed-up release key; the localhost evaluation key/feed is not a deployment.

## Later phase: update from inside prim and return to the room

Requested follow-up, explicitly outside the launcher MVP: while discussing
feedback together, a player can check for updates from the in-game menu, then
choose an update/restart action that returns them to multiplayer automatically.
Reserve the process/control boundaries now so this does not require redesigning
the updater later.

The launcher/supervisor owns installation and restart; prim owns the menu, room
departure and application checkpoint. Connect them through a versioned local IPC
interface implemented through the existing Rust native extension: a Windows named
pipe restricted to the current user/session, and a Unix socket in a user-owned
runtime directory on Linux. Include a per-launch capability and request IDs;
requests are bounded, authenticated and idempotent. No listening public TCP port
or arbitrary shell-command endpoint is needed.
[Windows named-pipe access control](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights)

Initial message vocabulary can be small: `GetUpdateStatus`, `CheckForUpdates`,
`PrepareUpdate(release_id)`, progress/status events, and
`RestartIntoPreparedRelease(request_id, resume_handle)`. Keep the protocol reader
independent of the GUI window, and retain compatibility across a launcher update.
The updater accepts release IDs from its authenticated feed, not an arbitrary
download URL provided by another multiplayer peer.

Suggested flow:

1. The in-game button asks the launcher to check, and shows the release notes and
   size. A click queues a specific immutable release, so a second publication
   during the discussion cannot silently change the selected target.
2. Download and verify that release while the current game keeps running. Report
   progress in the in-game menu. A failed download does not close the game.
3. Once prepared, checkpoint local preferences, room identity/generation, launch
   mode and the latest accepted playback state. Preserve local-file mappings
   privately. Store an expiring, single-use resume record in per-user state;
   pass an opaque handle rather than a secret/media URL on the command line.
4. If this process is host, perform the planned handoff or coordinated room pause
   described below. Then acknowledge readiness, gracefully leave and exit. The
   supervisor waits for the actual process to end before activating the release.
5. Relaunch in the same desktop/VR mode, consume the resume record, reconnect to
   the same room generation and acquire the current authoritative snapshot. A
   running room's newer state wins over the client's old checkpoint.
6. Report ready only after app initialization and rejoin have completed. On failure,
   offer retry, previous version, or return to local mode without a restart loop.
   Keep the current join-muted default initially; restoring an open microphone is
   a separate UX choice. If a rolled-back client is incompatible with the room,
   explain that and retain local playback rather than repeatedly autojoining.

The existing `PRIM_AUTOJOIN=1` startup hook is useful groundwork, but it only calls
the ordinary join path. It does not identify an old room instance, retain playback
through a full process restart, or resolve simultaneous hosts. The new resume
record must include a compatible schema version and be excluded from support ZIPs.
If the launch includes a launcher self-update, persist the pending game restart
operation across that helper transaction too.

### What the current networking actually does

`docs/PROTOCOL.md` already documents that simultaneous first joins may split rooms
and that seamless migration/convergence is absent. Code inspection confirms:

- `discover()` performs two discovery attempts, then makes itself host. Once
  hosting, it publishes a room record with a 90-second expiry on an approximately
  20-second loop plus publication latency. It does not continuously discover and
  merge competing rooms during that loop.
- Every member derives the same DHT signing key from the lobby secret. Publication
  uses `client.publish(&packet, None)` and collapses failures into a generic retry
  status. The pinned pkarr 5.0.3 source supports compare-and-swap timestamps and
  distinguishes concurrency errors, but this call does not use those controls.
- A lower endpoint ID can win when already-hosting peers connect. That local
  tie-break does not guarantee that disconnected competing hosts find each other.
- Losing the current host calls `state.stop()`. Other clients leave the session;
  they do not elect a replacement while retaining the existing mesh.
- Playback generations/revisions and monotonic timestamps belong to the current
  host/process. They do not yet contain a durable room instance and leadership
  term suitable for migration.

Thus a group update exercises an existing correctness limitation, not merely a
download traffic spike. Random reconnect delay/backoff reduces contention but is
not a leader-election algorithm. Using pkarr CAS would improve publication-conflict
handling; it would not by itself provide a room membership agreement, durable
playback checkpoint, or partition-safe leader election.

Keep the two publications distinct: only the developer publishes release files;
clients read those immutable objects. Room-host DHT publication is the concurrency
problem. Atomic feed promotion and CDN caching solve a different problem.

### Recovery behavior to define before enabling group restarts

For a single client's restart, preserve the room and let that client catch up.
For a host's restart with other clients remaining, transfer a replicated playback
checkpoint and leadership before disconnecting, then have the old host return as
a normal member. If all clients restart, persist a shared paused checkpoint and
room generation outside their terminating processes. Resume after an authorized
new host has restored it; do not extrapolate an old process's monotonic clock
through downtime. Repeat clock synchronization under the new host.

Use `(room_instance, host_term, state_revision)` to order authority and shared
state. Clients must reject stale host terms and fence old writers after takeover.
Track each participant's intended release/protocol, distinguish planned restart
from ordinary departure, and bound how long to wait for a missing participant.
Compatible versions can update individually; a protocol-breaking update needs a
coordinated transition to a new compatible room generation. Do not implicitly
accept old/new protocol mixtures just because everyone shares the lobby secret.

### Coordination choices

The confirmed preference is **P2P eventually, with a small coordinator acceptable
at the early stage** to make rapid updates/restarts practical. Recommend that
coordinator for the first seamless-restart phase because it simplifies simultaneous
restarts substantially. The service is separate from the static update feed and
is not a requirement for the launcher MVP.

One process behind nginx, with a durable transactional store such as SQLite, can
serialize host acquisition, renewal and handoff for the private room. Persist a
room instance, increasing host term, lease owner, intended protocol/release and a
small latest committed playback/restart checkpoint. Authenticate room members;
keep checkpoints containing media URLs private with short retention and scrubbed
logs. Video and voice keep their existing paths; the coordinator carries no media.

Host acquisition and checkpoint writes must be conditional transactions, not a
read followed by an unconditional write. Validate a current lease/term on state
writes and require recipients to validate authority/expiry, so an old host cannot
continue accepted control after takeover. Preserve increasing terms across server
restarts and specify conservative lease expiration/restart behavior. Existing
local playback/voice can continue during a coordinator outage, but do not invent a
second authority by falling back to independent DHT elections when a lease cannot
be renewed. Room control/takeover may have to wait. These availability tradeoffs
and partition tests belong in a separate protocol design before implementation.

Keep room coordination behind an explicit boundary in the native networking code:
discover/join, acquire/renew/transfer authority, and checkpoint/recover state. Keep
the room-instance/term/revision and resume models independent of HTTP transport.
A future P2P implementation still needs its own agreement/partition protocol and
a deliberate versioned authority transition; it is not just an interchangeable
discovery URL. Do not run centralized and P2P election as competing automatic
fallbacks for the same room generation.

For the preferred eventual peer-to-peer design, the scope is larger: ongoing
discovery and split-room convergence, explicit membership, leadership terms,
replicated state/checkpoint acknowledgements, planned handoff, and a specified
partition/rejoin rule. A lowest-ID tie-break plus randomized retries can provide
best-effort convergence after connectivity returns; it cannot guarantee unique
authority across disconnected groups. Stronger guarantees require quorum-based
agreement and the associated minority-availability tradeoff. For an all-client
restart, surviving local checkpoints must be reconciled before any fresh host
starts publishing control state. Avoid treating timestamps or DHT CAS as a
substitute for that protocol.

Keeping an Iroh peer alive in a launcher-owned sidecar could eventually reduce
routine game-restart churn. It would move networking ownership out of the Godot
extension and still would not solve simultaneous cold joins or host-machine
failure. Defer that refactor unless independent networking lifetime becomes a
separate project goal.

Before claiming seamless group restart, stress six simultaneous cold joins, six
simultaneous update/rejoins, host-first/host-last departures, publication conflicts,
stale records, unequal download speed, one failed update/rollback, protocol mismatch,
leader crash during handoff, partitions and recovery, clock skew, service restart
if applicable, and the zero-live-client case. Assert one accepted authority after
convergence and preserved shared state, not just that every process reconnected.
