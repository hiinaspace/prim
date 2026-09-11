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
  Click the URL field for typing or use Paste URL. Movement keys are suppressed
  while editing text.
- **VR:** left stick moves; right stick turns; Y/B toggles the menu; the right
  controller's trigger activates its laser pointer. Select smooth turning and
  its speed in the menu, or leave snap turning enabled.

The playback bar supports clicking or dragging to an absolute position, with a
short debounce. Remote voices are HRTF-rendered at their avatar heads. Turning
pivots around the headset's ground position, including room-scale offsets, and
tracked controllers have box placeholders when the runtime supplies no visible
models. Movie speakers retain full volume through 3 m and fall by 6 dB for each
additional 3 m; the volume slider remains an independent overall adjustment.

Received voice volume and falloff are local listening preferences, saved between
runs. Defaults are 150% volume, full level within 3 m, and a smooth fade to silence
at 15 m. Set received volume to 0% to silence all remote voices without muting your
microphone or the movie. The two radius sliders maintain a valid inner/outer
interval. The floor is 20×20 m, and movement bounds are twice their original width
and depth around the same center; the screen, seats and other geometry retain
their original size and placement.

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
