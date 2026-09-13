# prim

A six-person Godot theater prototype with synchronized local video, Steam Audio
spatial voice, bundled VRM avatars, and a shared-secret Iroh/DHT lobby. Linux and
Windows builds include the patched Godot/libmpv runtime, yt-dlp and Deno.

Starts in singleplayer with the microphone muted. Desktop and PCVR share a world
menu for URL paste/text entry, playback, microphone selection/gain, name and
snap/smooth turning. One video is shared at a time; clients load it independently.
There is no media transfer, playlist, general multiplayer framework or UGC layer.

The Avatar tab selects Alicia or Vita, previews the full body, and measures or
adjusts standing eye height. RenIK uses the headset and hands; articulated fingers
and springbones run locally. See [avatars](docs/AVATARS.md) for the experimental
fit, asset provenance, and deferred features. All peers need the updated build.

For local Linux development (Nix required):

```sh
./build.sh
./run.sh             # VR; start your headset runtime first
./run.sh --desktop   # desktop mode
./run.sh --editor    # patched Godot editor
```

The build stages the patched engine, media/audio extensions and viseme runtime.
Repeat it after native changes; GDScript changes need only a restart. Use
`./build.sh --refresh-deps` after changing pinned engine/media dependencies. The first
build can take a while; subsequent builds reuse Nix and Cargo caches.

See [building](docs/BUILDING.md), [protocol boundaries](docs/PROTOCOL.md), and
[private build testing](docs/TESTING.md). Automated Linux, six-client, public-DHT
and mixed Linux/Windows-under-Wine checks have passed. Native Windows and headset
validation are manual gates.

A lost host returns clients to local playback; reconnecting can establish a new
room. Build outputs and the generated lobby secret are excluded from Git. The
secret belongs only in private packages, never DHT records or diagnostic logs.
