# prim

A six-person Godot theater prototype with synchronized local video, Steam Audio
spatial voice, bundled VRM avatars, and a shared-secret Iroh/DHT lobby. Linux and
Windows builds include the patched Godot/libmpv runtime, yt-dlp and Deno.

Starts in singleplayer with the microphone muted. Desktop uses a 2D menu; PCVR
uses a panel drawn over scene geometry. Both offer URL paste/text entry,
playback, microphone selection/gain, name and
snap/smooth turning. One video is shared at a time. URLs load independently; any connected participant can use
**Movie → Browse… → Share with room** to replace the movie with a local video
streamed over Iroh. Local selection offers Share with room or Play only here;
file drops use the same chooser. Received bytes
use a 32 MiB temporary RAM cache. Relayed viewers need the file provider's approval
before media flows. The room host continues to order playback and synchronization.
Movie offers local **Direct stereo** movie audio and embedded subtitle selection.
Turning settings are in **Comfort**. **Enable VR / Disable VR** switches modes while
keeping the Prim room alive; see [session lifecycle](docs/XR_LIFECYCLE.md).
Live RTSP/RTMP and unbounded streams bypass movie seeking/synchronization; see
[the manual OBS/MediaMTX test](docs/LIVESTREAM_TEST.md).
There is no playlist, general multiplayer framework or UGC layer.

The Avatar tab selects Alicia or Vita, previews the full body, and measures or
adjusts standing eye height. RenIK uses the headset and hands; articulated fingers
and springbones run locally. See [avatars](docs/AVATARS.md) for the experimental
fit, asset provenance, and deferred features. All peers need the updated build.

For local Linux development (Nix required):

```sh
./build.sh
./run.sh             # try VR, fall back to desktop
./run.sh --desktop   # start desktop; Enable VR later
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

A lost host stops shared-file playback and preserves ordinary local/URL playback; reconnecting can establish a new
room. Build outputs and the generated lobby secret are excluded from Git. The
secret belongs only in private packages, never DHT records or diagnostic logs.

Implementation and future media work: [direct file sharing and OBS livestreaming](docs/DIRECT_MEDIA_PLAN.md),
with relay-use confirmation and a separate later overlay/remote-play track.
