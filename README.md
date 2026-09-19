# prim

Prim is a shared virtual living room: talk with friends, inhabit avatars, watch
media, and stay together across activities. See [project direction](docs/DIRECTION.md)
for the longer-term design and [contributing](CONTRIBUTING.md) for useful work.
Agent-assisted fixes and exploratory draft PRs are welcome; start with
[AGENTS.md](AGENTS.md). The official user-facing build is maintainer-signed and
available at [prim.hiina.space](https://prim.hiina.space/).

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
[testing](docs/TESTING.md). Automated Linux, six-client, public-DHT
and mixed Linux/Windows-under-Wine checks have passed. Native Windows and headset
validation are manual gates.

A lost host stops shared-file playback and preserves ordinary local/URL playback; reconnecting can establish a new
room. Build outputs and the generated lobby secret are excluded from Git. The
development secret creates an isolated test room. The initial public testing
arrangement is a single shared friends-scale lobby; sharing its configuration
does not provide private membership. Secret values stay out of DHT records and
diagnostic logs. See [release and lobby policy](CONTRIBUTING.md#releases-compatibility-and-the-shared-lobby).

Implementation and future media work: [direct file sharing and OBS livestreaming](docs/DIRECT_MEDIA_PLAN.md),
with relay-use confirmation. Companion/peek paths are experimental; integrated
capture, richer spectator output and remote play remain separate future work.

Longer-term design: [the persistent living room and external experiences](docs/EXPERIENCE_CONTINUITY.md)
records desktop/XR transitions, activity-aware voice, trusted game launch, and
the first mech-vr solo spectator use case. It does not change current priorities.

Original Prim source is [Unlicense](UNLICENSE). Third-party code, adapted viseme
code and bundled avatars retain their own terms; see [licensing and attribution](LICENSES.md).
