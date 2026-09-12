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
movie. Each client needs independent access to the source; site restrictions or
mismatched local files are not solved by synchronization.

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
