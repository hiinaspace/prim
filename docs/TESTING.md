# Building and testing a candidate

Use one matching candidate for all participants; see [protocol](PROTOCOL.md) for
the current wire version and [building](BUILDING.md) for source setup. For the
official signed build see [the download page](https://prim.hiina.space/).
The records below describe particular earlier revisions, not a fresh test of
your checkout. Current companion follow-up is in [COMPANION_NEXT_PLAN.md](COMPANION_NEXT_PLAN.md).
Native Windows and individual runtime/headset combinations remain distinct checks.

Extract the whole archive. Matching friends packages use a shared lobby configuration;
source builds generate an isolated one. See [lobby policy](../CONTRIBUTING.md#releases-compatibility-and-the-shared-lobby).
Start in singleplayer; the microphone is muted until explicitly enabled after
connecting. Allow the application through the firewall when prompted.

- **Linux:** `./desktop` for desktop mode; `./prim` for PCVR with an active OpenXR runtime.
- **Windows:** `Desktop.bat` or `VR.bat`. If a Visual C++ runtime DLL is missing,
  run the included `VC_redist.x64.exe` installer. It comes from Microsoft's
  [supported runtime download](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist).
- **Desktop:** WASD moves and the mouse looks while the menu is closed. Tab or
  Esc opens/closes the 2D menu; use the mouse directly on its controls.
  With the menu closed, refocusing the window or clicking back inside it
  recaptures the cursor. With the menu open, the pointer remains available.
  Click the URL field for typing or use Paste URL. Movement keys are suppressed
  while editing text.
- **VR:** left stick moves; right stick turns; Y/B toggles the menu; the right
  controller's trigger activates its laser pointer. Select smooth turning and
  its speed in the menu, or leave snap turning enabled. With only one tracked
  controller (either hand), its stick moves forward/back relative to your head
  and turns horizontally. Its trigger controls the menu laser. A/X on either tracked controller toggles
  microphone mute while connected, including with the menu closed.

The playback bar supports clicking or dragging to an absolute position, with a
short debounce. Remote voices are HRTF-rendered at their avatar heads. Turning
pivots around the headset's ground position, including room-scale offsets, and
tracked controllers have box placeholders when the runtime supplies no visible
models. Movie speakers default to full volume through 6 m and a smooth fade to
silence at 18 m. The Video section has independent Full volume within / Silent
beyond sliders, saved locally between runs. Movie distance filtering and air
absorption are disabled: both distance and the main volume slider reduce level
without muffling. The two radius sliders keep a valid inner/outer interval.

Received voice volume and falloff are local listening preferences, saved between
runs. Defaults are 150% volume, full level within 3 m, and a smooth fade to silence
at 15 m. Set received volume to 0% to silence all remote voices without muting your
microphone or the movie. Remote nameplates turn green with a Speaking label
while decoded voice is active, with a 200 ms hold across speech gaps. This is
an audio-activity indicator (background noise can trigger it), measured before
your receive gain or distance attenuation. The two radius sliders maintain a valid inner/outer
interval. The floor is 20×20 m, and movement bounds are twice their original width
and depth around the same center; the screen, seats and other geometry retain
their original size and placement.

The read-only Current source field shows the active URL or your selected local
path separately from the editable URL draft. Source changes are also written to
stderr/the Godot log with a `[prim media]` prefix. For shared local files without
a local selection, only the shared filename is available. Joining an idle host
stops any movie you were playing locally and clears this field.

First test a familiar URL or local file alone, including pause, seek, subtitles,
movie volume and closing/reopening the menu. Then have one person press Connect
and wait for the lobby to publish (initial discovery can take tens of seconds).
Other friends press Connect afterward. Change the display name, select the mic,
set gain while watching the meter, then unmute. Use headphones for this prototype;
there is no acoustic echo cancellation.

With two people, check voice in both directions while moving/turning, head and
hand tracking, nameplates, shared URL changes and pause/seek/resume. Let a third
person join during playback. Test mute/unmute and switching microphones. Close
the host: clients should return to local playback muted with remote avatars gone.
Reconnect deliberately to start another room. Finally try six people and a longer
movie. URL viewers need independent access to the site. For a local video, any participant can use
Movie → Browse… → Share with room; viewers receive its bytes automatically.
Choose Play only here for independent local playback.

## Automated evidence for the initial build

- Native six-peer full mesh, membership/departure/host loss, wrong-secret rejection.
- Public DHT host discovery without exchanging addresses out of band.
- Six Linux Godot processes with real decoded video, mixed synthetic voice and pose updates.
- Linux/Windows-under-Wine pair: video synchronization, bilateral voice, pose,
  pause/seek/resume, mute/unmute and process-scoped main-thread stall recovery.
- Packaged Linux and Windows-under-Wine video rendering, speed/pause observations and clean exit.
- Real YouTube URL playback through bundled yt-dlp/Deno on Linux and Windows under Wine.
- Desktop menu ray selection, text entry, smooth-turn setting and head-pivot turning.

Still manual: native Windows startup/devices, both PCVR runtimes, headset laser
and dropdown usability, subjective spatial audio and long-session comfort,
Internet NAT/relay behavior with friends, and other Linux distributions. Wine
success is useful coverage but does not establish native Windows PCVR support.

## Latest friend-feedback regression checks

- Six Linux clients and a Linux/Windows-under-Wine pair: pre-join local playback
  stops for an idle host, then shared source display, video sync and voice pass.
- Desktop click/refocus capture and either-hand single-controller axis mappings.
- Movie output spectrum: lowering the slider by 18 dB scales 500 Hz and 6 kHz
  equally; at 12 m the new default distance curve halves both bands equally.
- Menu rendering, movie radius controls, existing scrubbing, voice controls and
  body-pivot checks. Controller mute toggles only on the press edge, and decoded
  remote audio lights the talking indicator in the multiplayer test.

Please verify single-controller tracking transitions and menu use in a headset,
and compare movie volume at a fixed seat versus walking away from the screen.

## Direct file sharing

In the current checkout, connect, open **Movie → Browse… → Share with room**,
and use **Sharing** for the aggregate upload limit and per-viewer relay approval.
Any participant can provide the file; the room host still orders playback.
Re-sharing resets approval. Stop sharing or leave the room to revoke the file.
Use matching clients; [protocol](PROTOCOL.md) describes the current handshake.
The evidence below was recorded during the initial host-only sharing experiment;
later participant-sharing checks appear further down this document.

Reproducible checks, after staging the patched Godot/libmpv runtime as described
above (set `GODOT` and `LIBMPV_ZERO_MPV_LIBRARY` for custom runtime locations):

```sh
nix develop --command cargo test --lib
nix develop --command cargo clippy --all-targets -- -D warnings
PRIM_TEST_SHARED_FILE=1 nix develop --command python3 tools/test-integration.py
PRIM_TEST_SHARED_FILE=1 PRIM_TEST_CONTAINER=mkv PRIM_TEST_PEERS=5 nix develop --command python3 tools/test-integration.py
```

Verified 2026-09-13 in the isolated worktree:

- Native tests: 7 passed, public DHT test intentionally ignored. Real local Iroh
  ranges match fixture bytes across block boundaries; repeated reads hit cache;
  suffix/HEAD/416, stale capabilities, replacement consent reset, changed-file
  rejection, cache bound and stop revocation pass. Synthetic selected-path
  overrides exercise direct-to-relay gating and approval over real connections;
  they are **not** an actual relay migration test.
- Two Linux Godot processes play the shared MP4; six play a shared MKV with an
  embedded subtitle track through the existing mpv adapter. Guests report received
  media bytes; playback, pause/seek/resume, voice and avatar assertions pass.
  The final six-process initial drift samples were 44–170 ms. These are local
  desktop measurements, not an Internet latency or sustained-throughput claim.
- Menu regression passes, including five relay rows and an approval click scoped
  to the intended viewer/share; the rendered Sharing tab was visually inspected.
- Rust formatting and Clippy with warnings denied pass. `cargo check --target
  x86_64-pc-windows-gnu` passes using the existing Windows toolchain with isolated
  outputs; this checks compilation, not Windows linking or runtime playback.

Local evidence is in `.local/media-final-native.log`, `.local/media-six-final2.log`,
`.local/media-menu.log` and `.local/media-menu-sharing.png` (ignored build outputs).

Remaining qualification: actual forced relay and direct/relay migration, WAN
loss/latency and long high-bitrate playback, late joins while streaming, native
Windows runtime and both PCVR runtimes, headset file selection, subtitle visual
selection and multi-track/audio behavior. Existing Wine/VR evidence elsewhere in
this document predates this feature. Source mutation checks are best effort;
do not edit an actively shared file. Live OBS ingest, capture and overlays are
not implemented by this file-sharing change.

### Combined viseme and streaming release checks

Rebased onto local main `cd2138f`, including visemes (`31ff698`) and launcher
packaging. GNA remains pinned at `d9b61bc`. Native tests: 8 passed, one public DHT
test ignored; Clippy and Linux/Windows native builds pass. Six Linux peers pass
streaming, clock, voice, avatar and viseme lifecycle assertions together. A Linux
host and the exported Windows-under-Wine guest also pass those checks after
including the MinGW runtime DLL. The packager now rejects unresolved compiler
runtime imports instead of treating them as Windows system libraries.

The 0.3.5 Linux AppImage passes all 23 launcher self-checks, installed-file
verification and a 90-frame desktop launch/exit on Ubuntu 26.04.1 with `/nix`
hidden. The isolated desktop menu regression also passes. These checks do not
establish real WAN relay migration or native Windows/headset behavior. No new
claim about subjective viseme timing is made.

## Playback/provider follow-up (2026-09-15)

Run the staged development build after rebuilding native extensions:

```sh
nix develop --command cargo test --lib
nix develop --command cargo clippy --all-targets -- -D warnings
# Set GODOT and LIBMPV_ZERO_MPV_LIBRARY to the staged patched runtime.
nix develop --command python3 tools/test-playback-controls.py
PRIM_TEST_SHARED_FILE=1 PRIM_TEST_PROVIDER=client0 PRIM_TEST_PEERS=5 PRIM_TEST_CONTAINER=mkv nix develop --command python3 tools/test-integration.py
PRIM_TEST_SCRIPT=sharing.gd PRIM_TEST_PEERS=2 PRIM_TEST_CONTAINER=mkv nix develop --command python3 tools/test-integration.py
./run.sh --desktop --script res://tests/runtime_vrm.gd
```

Use isolated `XDG_DATA_HOME` for direct test-script launches and a private X display
when possible. The Python control/room runners create isolated audio sinks and
preferences. Do not run two room runners simultaneously: they share their output
folder. `PRIM_TEST_WINDOWS_EXE` runs the exported Windows test package under Wine;
use an isolated Wine prefix and export the corresponding test script first.

Treat engine/script errors in logs as failures even if a test prints PASS.
Runtime tests also check that every imported spring initializes in repeated
avatar instances; exported packages exercise script remapping separately.

Focused control fixtures exercise channel isolation, sample alignment, 5.1 center downmix, output
switching, pause/seek, movie gain, subtitle enumeration/selection/Off, rendered
subtitle pixels (including paused Off), no-subtitle replacement, quoted/URI paths,
and simulated drops without implicit playback replacement.

Sharing fixtures exercise non-host providers, late join, replacement, competing
offers, stale acknowledgements/stops, ownership spoof rejection, provider/host
stop, provider departure, URL transitions, and independent subtitle choices.
Native range tests retain byte equality, bounded cache, mutation rejection,
revocation and controlled relay-policy checks; new preparation tests preserve
an incoming stream and active publication during replacement/cancellation/errors.

Runtime-import fixtures copy raw VRMs without import sidecars, check normalized
fingers, first/third-person mesh data, spring initialization against actual bone
indices, finite solved poses, and no repeated reset during ordinary movement.
For exported packages, pass `PRIM_RUNTIME_VRM_FIXTURE` pointing to an external raw
VRM file without an import sidecar. These fixtures do not prove all VRM versions
or arbitrary proportions work.

Manual follow-up: compare Direct stereo/Screen speakers in the headset; try the
actual OS picker and desktop file drop on Linux/native Windows; compare local and
remote springbone behavior with telemetry; exercise real WAN relay fallback and
long playback. Wine checks are not native Windows/PCVR qualification.


## Restartable OpenXR candidate — 2026-09-17

See [desktop/VR lifecycle](XR_LIFECYCLE.md) for the implementation, exact staged
engine, artifacts, and afternoon human test card. The final Linux Nix engine
passed 137 checks using two Prim processes and an isolated Monado QWERTY/null
compositor, including repeated entry/exit, runtime loss/restart, head/controller
tracking, continuous remote voice/media/poses and graceful close from VR. The
expanded suite reproduces the original re-entry defect on the old engine and
checks actual render-buffer/scene view counts and eye separation on the patched
engine. All six entries retain stereo, desktop exits return to mono, and rendering
errors fail the runner. The
36 menu, 29 playback-control and 23 launcher checks also passed. This is not
physical-headset or native-Windows evidence, and existing friend packages were
not rebuilt. `nix develop --command python3 tools/test-xr-lifecycle.py` reproduces
the runtime test without touching the normal VR service or default audio devices.


## Spatial movie/voice audio — 2026-09-17

See [spatial audio diagnosis](SPATIAL_AUDIO_DIAGNOSIS.md). The deterministic
white-noise regression compares first-order Ambisonics, point-source HRTF and
bypass; it checks spectral loss independently from split-channel timing, verifies
left/right and head-turn localization, and captures identical split channels
before HRTF on two output-mode transitions. Final captures are sample-identical.
The rebuilt Steam Audio and libmpv-zero plugins also pass the 29 playback controls.
Run `nix develop --command uv run --with numpy python tools/test-spatial-audio.py`.
The concurrent XR loss-injection run coincided with NVIDIA Xid 51/154 and a
persistent reset-required state; a later isolated run also failed Vulkan startup.
Final XR qualification is blocked on driver recovery. Do not repeat loss injection
on the live GPU until investigated; details are in the diagnosis note.

## September 17 menu and live receiver follow-up

See [the OBS/MediaMTX test card](LIVESTREAM_TEST.md) for the current Room/Movie
layout, unified file chooser, local-only semantics, RTSP/TCP URL, test results and
remaining human gates. Earlier world-ray desktop menu and separate Sharing
picker instructions above are historical.


## Monado companion overlay

See [XR_COMPANION_TEST.md](XR_COMPANION_TEST.md) for the headset card and exact
verification boundaries. `nix develop --command python3 tools/test-xr-companion.py`
starts its own Monado/QWERTY runtime, a main XR app and two Prim clients; it does
not stop the host runtime. Gesture and policy tests are `res://tests/xr_gesture.gd`
and `res://tests/xr_policy.gd`. Always set an isolated `XDG_DATA_HOME` for UI tests;
Linux `res://tests/menu.gd` now refuses to run without one because it saves controls.

## SteamVR companion

`tools/test-steamvr-companion.py --allow-scene-takeover` exercises real Linux
SteamVR background tracking and ordinary OpenXR handoffs, with a second local
Prim client checking avatar delivery. It deliberately replaces the headset
scene, uses isolated profiles and cleans up its disposable applications. Build
both `tools/build-steamvr-probe.sh` and `tools/build-openvr-helper.sh linux`
first. Wear the headset and hold both controllers for the grip-origin diagnostic;
unworn/standby action poses are not useful for that comparison.

`res://tests/steamvr_policy.gd` covers ownership, stale data and pose conversion
without a runtime. See [STEAMVR_COMPANION_PLAN.md](STEAMVR_COMPANION_PLAN.md) for
local evidence and the short Windows friends checklist. Linux live tests and Wine
loading are not native Windows headset qualification.

## Companion follow-up controls and visibility

- `nix develop --command python3 tools/test-monado-status.py`: rejected status
  library + runtime-local Envision-layout fallback, without using real IPC.
- `tools/test-xr-companion.py`: private Monado real sessions, placement 1/10,
  XR-only composition restart, neutral-gated peek sticks and explicit-hide latch.
- `project/tests/menu.gd`: isolated-profile menu regression, independent deafen
  buses, tracked mouse yaw/head pivot and pointer focus behavior.
- `project/tests/wrist_controls.gd`: dwell/rearm arbitration; `wrist_render.gd`
  writes a layout preview to `PRIM_WRIST_IMAGE` using the actual 3D UI.
- With SteamVR already running and headset/controllers active,
  `nix develop --command python3 tools/test-steamvr-companion.py --allow-scene-takeover --peek`
  checks the experimental projective overlay and OpenXR scene handoffs.
  `PRIM_PEEK_INTERACTIVE=1` adds a two-minute headset test. This replaces the
  current scene with a disposable rendering reference game. SteamVR's Linux
  launcher requires the Steam client/runtime launch service; a bare ownership
  probe displaying “Next up” is not a rendering or controller-input test.

[Arch/Envision retry instructions](ARCH_COMPANION_TEST.md) identify the remaining
friend-machine composition check. OpenVR Linux headset success and Windows
cross-build success are separate from Windows headset, other controller profiles,
and performance qualification. Deafen needs a friends/audio listening check in
addition to assertions on the final output buses.
