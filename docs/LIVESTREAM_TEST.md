# Manual OBS / MediaMTX friend test

The September 17 working tree supports live reception alongside ordinary movies.
Start the staged Linux checkout with `./run.sh --desktop` and open **Movie**.
Use the same candidate on every room participant; older builds lack this mode.

## Publish and receive

Use a MediaMTX server you control and whose RTMP/RTSP listeners are reachable by
the intended viewers. Substitute its hostname, configured ports and test stream
path for `media.example.test:1935`, `media.example.test:8554` and `prim-test` below;
these are placeholders, not a project-provided service. Supply any authentication
required by your server. In OBS, use Custom streaming service:

- Server: `rtmp://media.example.test:1935`
- Stream key/path: `prim-test`
- For this first test use H.264 video and AAC audio.

Start streaming, then paste **`rtsp://media.example.test:8554/prim-test`** into Movie and Open.
Use your configured listener ports. Prim explicitly selects TCP for
RTSP; the URL remains `rtsp://`, without VRChat's `rtspt://` spelling.
Standalone comparison:

```sh
mpv --no-config --rtsp-transport=tcp rtsp://media.example.test:8554/prim-test
```

A 404 before the publisher starts is expected. Start OBS and use **Reconnect
live** if Prim was opened first. Reconnect affects only that viewer.

Live sources share the URL, not an exact movie clock. Each viewer follows its
own live edge; scrub is disabled and the button becomes Reconnect live instead
of room pause/play. RTSP/RTMP are live sources. Other sources are recognized from
unknown duration, missing full seekability, or cache-only seekability. mpv can
report a growing duration for a live source, so duration alone is insufficient.
This conservative policy also avoids synchronizing nonseekable HTTP media.
Finite, fully seekable media retains room pause, seeking and drift correction.
There is no automatic reconnect loop, OBS controller, capture, private live
transport, or measured/tuned capture-to-display latency in this slice.

## Menus and local files

Desktop uses a centered 2D menu with a normal cursor; Tab/Esc opens/closes it.
VR uses the same controls on a world panel rendered over scene geometry, with
its pointer above the panel. **Room** holds connection, voice, a larger mute
button, and a separate Enter/Leave VR row. **Movie** holds source, playback,
subtitles, stereo and movie volume controls. **Sharing** holds transfer status,
upload limit and per-viewer relay approval.

Movie's Browse, pasted paths and file drops use one chooser. In a room, choose
**Share with room**, **Play only here**, or **Cancel**. Solo selection opens the
file directly. Local-only playback does not change room source, pause or seek;
a host keeps publishing the room's independent clock. **Return to room
playback** reloads the current room source and position. A new room source also
returns participants to shared playback. Selecting a local file never grants
byte-transfer permission without choosing Share with room.

## Evidence and next manual checks

Linux desktop automation in `.local/menu-live/`:

- 75 menu checks: actual 2D mouse/keyboard dispatch, settings, sharing controls,
  and three 2D/world-menu transitions without starting an XR runtime, including
  ordinary button release and embedded dropdown selection in both directions.
  Transitions are triggered inside the Enter/Leave button callback. The menu
  now keeps one standalone viewport; desktop mouse/keyboard and VR ray events
  are forwarded explicitly, with no viewport reparenting on a mode change.
- 29 existing playback controls: subtitles, stereo/downmix/channel alignment,
  file URI normalization and the multiplayer selection prompt.
- 14 media policy checks: unknown/growing duration, cache-only seeking, live
  pause/seek bypass without clock lock, return to VOD, malformed live metadata,
  and host clock continuity while an asynchronous restore seek is pending.
- Two real Prim processes: local-only host and guest isolation and return to
  the current room clock; temporary test-pattern RTMP publish through chirashi
  and RTSP/TCP receive, live reconnect, and return to synchronized finite media.
- Three real Prim processes: Movie-tab share prompt, participant providers,
  late join, embedded subtitles, replacements/competing offers and departure.

The temporary publisher used `prim-agent-check-0917`, then was stopped; `/test`
was left for the user's OBS session. Standalone mpv decoded that stream too.
Test sessions used private settings and audio sinks. Engine/runtime-loss tests
were not repeated. The user subsequently confirmed livestreaming worked well and VR menu input
worked after the first correction. The remaining desktop-return bug was Godot
retaining the headset-sized main viewport after disabling XR. Resizing the live
window restored its menu size and the user confirmed mouse input recovered.
The exit path now reapplies the existing Window content settings. A size-only
XR interface reproduces the old failure and verifies three returns with different
content scales, without starting or stopping a real runtime. Headless and rendered
checks pass. The user subsequently accepted the automatic fix and this batch's
single-client testing; Linux/Windows 0.3.6 is published for friends.
Evidence: `.local/menu-desktop-return/viewport-*.log` and live screenshots.
Native Windows, native picker interaction, game-only audio
routing and measured end-to-end latency retain separate human gates.

For the first friend session: publish game audio only, open the live URL on both
peers, keep Prim voice active, reconnect one viewer, then switch to a finite
subtitled movie and confirm pause/seek returns. Compare standalone mpv if there
is extra delay. Tracked presence while another game owns XR remains separate.

Reproduce focused tests after staging the native extension:

```sh
# Use the runtime environment from run.sh / docs/BUILDING.md.
GODOT="$PWD/.local/godot/bin/godot" PRIM_TEST_SCRIPT=media_modes.gd \
  PRIM_TEST_LIVE_URL=rtsp://media.example.test:8554/prim-test \
  nix develop --command python tools/test-integration.py
# Omit PRIM_TEST_LIVE_URL to test finite/local-only playback without a publisher.
```
