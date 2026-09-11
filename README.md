# prim

Private six-person Godot theater prototype: local video, synchronized playback,
spatial voice, head/hand avatars, and a shared-secret Iroh/DHT lobby.

Starts in singleplayer with the microphone muted. Desktop and PCVR share a
world-space menu. The initial source controls support URL paste and desktop
text entry, with one shared video and no media transfer or playlist.

## Implementation gates

- [ ] Pin/build the Linux and Windows native dependencies.
- [ ] Singleplayer theater, desktop/VR pointer menu, movement and turning settings.
- [ ] Asynchronous room discovery, authenticated Iroh connections and spatial voice.
- [ ] Host-ordered playback, clock estimates, late joins and drift recovery.
- [ ] Two- and six-client automated checks, failure/rejoin and scoped stress.
- [ ] Private packages, Wine smoke test, then manual native Windows/PCVR tests.

This is a prototype, not a general multiplayer framework. A lost host returns
clients to local playback; reconnecting can establish a new room. Room membership
and playback policy belong here; voice codecs/playout and local video playback
remain in their respective dependencies.

Build configuration and generated lobby secrets are local artifacts, excluded
from version control. Do not put the lobby secret in DHT records or diagnostic logs.
