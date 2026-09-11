# Prototype boundaries

One native Iroh endpoint carries three kinds of traffic: bounded reliable control
messages, lossy sequenced pose datagrams, and encoded voice datagrams. Voice
capture/encoding and receive ingress run independently of Godot's main thread.
The decoder/playout stream feeds Steam Audio at each remote avatar's head.

The shared secret derives the DHT record signing key. Records contain expiring
endpoint addresses, not the secret. Connections prove knowledge of the secret
using TLS exporter material before accepting membership or application traffic.
The record is a discovery hint, not an admission credential. Peers form a full
mesh capped at six people. A single host orders playback changes. This is a
trusted-friends protocol, with no roles or moderation framework.

The host publishes media generation, state revision, sample sequence, position,
pause state and monotonic sample time. Four-timestamp clock probes estimate the
host clock offset using the lowest observed RTT. Clients independently load the
original URL (including yt-dlp resolution), estimate the target position, and use
bounded speed corrections for small drift or exact seeks for large drift. Slow
clients catch up; they do not pause the whole room. A local file is identified by
basename and each client supplies its own matching file; bytes are never relayed.
Subtitle selection remains local to mpv's default track selection for this MVP.

Poses are versioned fixed-size binary records with sequence, tracking flags, and
head/left/right transforms. Receivers validate values, reject old sequences and
interpolate. Names travel on the reliable channel. Desktop users have a head
pose; hands are hidden when untracked.

Joining starts muted. Leaving or losing the host stops capture, removes remote
avatars and preserves local movie playback. Reconnecting can create a new room.
No seamless host migration or ongoing split-room convergence is implemented;
simultaneous first joins can produce separate rooms. For the friend test, let one
person connect first and wait for lobby publication before others connect.

Bounded channels and coalesced pose updates prevent an unbounded slow-consumer
backlog. Losing critical native events fails the session visibly rather than
silently keeping an inconsistent membership state. This protocol intentionally
has no general Godot MultiplayerAPI adapter, media transfer, queue or UGC layer.
