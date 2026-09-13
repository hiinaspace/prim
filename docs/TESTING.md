# Private build test

Extract the whole archive. Both packages share one private lobby configuration.
Start in singleplayer; the microphone is muted until explicitly enabled after
connecting. Allow the application through the firewall when prompted.

- **Linux:** `./desktop` for desktop mode; `./prim` for PCVR with an active OpenXR runtime.
- **Windows:** `Desktop.bat` or `VR.bat`. If a Visual C++ runtime DLL is missing,
  run the included `VC_redist.x64.exe` installer. It comes from Microsoft's
  [supported runtime download](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist).
- **Desktop:** WASD moves; mouse looks; center dot aims at the menu; left click
  activates controls. Tab opens/closes the menu; Esc releases/captures the cursor.
  Refocusing the window or clicking back inside it recaptures the cursor; that
  click does not also activate a menu control.
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
movie. URL viewers need independent access to the site. For a host-owned video, use
Sharing → Share file; viewers receive its bytes automatically. The original
local-copy mode still requires matching local files.

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

## Direct file sharing worktree

In `/home/s/code/prim-streaming-plan` (`codex/direct-media-plan`), connect as host,
open **Sharing**, choose **Share file** (desktop native picker) or paste a path,
and set the aggregate upload limit. Guests resolve the descriptor automatically.
If a viewer needs a relay, the Theater status points to Sharing; choose Allow or
Direct only for that viewer. Re-sharing resets approval. All clients need hello
version 3. Stop sharing or leave the room to revoke the file.

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
