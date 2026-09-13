# Prototype boundaries

One native Iroh endpoint carries room traffic: bounded reliable control
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
basename when using the original local-copy mode. The Sharing tab instead
streams the host's file to viewers over a dedicated authenticated connection.
Subtitle selection remains local to mpv's default track selection for this MVP.

The authenticated connection hello is version 3; older builds must be updated.
Reliable `avatar_state` carries `avatar` (a catalog ID), `revision` (catalog
revision 1), `eye_height` (0.5–2.5 meters), and a uint32 configuration `epoch`.
Unknown IDs or catalog revisions use cubes. A body swap preserves the peer's
head/voice anchor, receive stream, name and talking indicator. No paths or avatar
bytes are sent.

Pose v2 is a 636-byte, little-endian, self-contained snapshot at 20 Hz:

| Offset | Content |
| --- | --- |
| 0 | uint32 version, sequence, tracking bits (left=1, right=2, head=4) |
| 12 | uint32 configuration epoch, discontinuity epoch, sender milliseconds |
| 24 | two uint32 validity masks, 15 bits per hand |
| 32 | head/view, left wrist, right wrist: float32 XYZ + quaternion XYZW each |
| 116 | 30 float32 XYZW quaternions: left then right, 15 joints per hand |
| 596 | ten float32 fallback curls: thumb through little, left then right |

Finger orientations are relative to the corresponding tracked wrist, in Godot's
humanoid axes. Ordering: thumb metacarpal/proximal/distal; then proximal,
intermediate, distal for index, middle, ring, little. Positions and hand
metacarpal translations are not transmitted. The applicator converts through the
actual parent hierarchy, including Alicia's shared helper bone. This preserves
thumb opposition and spreading; curls are only an untracked-joint fallback.

Receivers validate size/version, finite values, quaternion norms, position bounds,
flags and curls. Sequence comparisons handle uint32 wraparound. A single latest
pose may wait up to three seconds for its reliable configuration. Remote targets
are interpolated before local IK; the owner's render-frame targets bypass that
network smoothing. After 350 ms without a pose, remote hands relax; after three
seconds the avatar hides. Discontinuities reset placement and springs. Complete
remote bodies hide within 35 cm of the viewer, returning beyond 40 cm, so shared
spawns cannot surround the camera with a head mesh. Voice remains at the tracked
head. Desktop peers send a head pose and use inferred relaxed arms/legs.

Raw pose payload is about 12.7 KB/s per peer (63.6 KB/s outbound to five peers),
before transport overhead. Body bones and spring state are solved independently
by each receiver. A future sender-solved body mode will need a separate negotiated
schema and capability; it is not implemented or reserved as arbitrary payloads.

Joining starts muted. Leaving or losing the host stops capture, removes remote
avatars and preserves ordinary local/URL movie playback. Peer-file playback
stops, and its share capabilities and cache are revoked. Reconnecting can create a new room.
No seamless host migration or ongoing split-room convergence is implemented;
simultaneous first joins can produce separate rooms. For the friend test, let one
person connect first and wait for lobby publication before others connect.

Bounded channels and coalesced pose updates prevent an unbounded slow-consumer
backlog. Losing critical native events fails the session visibly rather than
silently keeping an inconsistent membership state. This protocol intentionally
has no general Godot MultiplayerAPI adapter, playlist or UGC layer.

## Host file sharing (range protocol 1)

`peer_file` playback sources contain JSON `{id, owner, name, size}`. The ID is an
opaque random 64-hex share generation, owner is the host endpoint ID, name is a
sanitized display basename, and size is a decimal **string** (Godot JSON numbers
are doubles). No host path or receiver-local URL is broadcast. Re-sharing the
same file creates a new ID and clears relay consent.

The existing endpoint dispatches `prim/media-range/1` to a separate connection.
It requires an active room member and a keyed BLAKE3 proof over the connection's
TLS exporter (`prim-media-auth-v1`), using the room secret. Only the current host
serves files. One dedicated media connection per viewer admits at most four
concurrent range streams, separate from room control and voice queues.

After the proof stream, each bidirectional stream requests 64 ASCII ID bytes,
a big-endian u64 offset and u64 length (1–1,048,576 bytes). The response starts
with one byte: 0 success followed by exactly the requested bytes, 1 source gone,
2 waiting for path/consent, 3 relay declined, 4 invalid range. Interruptions never
insert partial blocks into the cache. Open-file size/mtime checks reject changed
sources; this is a trusted-file workflow, not cryptographic snapshot validation.

Selected-path notifications distinguish direct, relay and unknown. Unknown and
unapproved relayed paths carry no new range payload. The host approves/declines
per viewer and share in Sharing; direct-to-relay changes reapply the same gate.
Checks run before each 64 KiB write, with a configurable aggregate application
payload cap (default 100 Mbit/s). Transport overhead is additional. Asynchronous
path observation and already queued QUIC bytes mean this is not a guarantee of
zero relay bytes during migration. Voice and room membership remain connected.

Each receiver exposes an ephemeral IPv4 loopback HTTP adapter with a private
random capability path, exact Host validation, GET/HEAD, single byte ranges,
206/416 and exact Content-Length. Multiple ranges are ignored with a full 200.
It serves only the current descriptor to the existing mpv decoder. Eight HTTP
requests share a 32 MiB RAM block cache; mpv and bounded in-flight buffers use
additional memory. There is no disk cache, persistent download or swarm.
Stop, source replacement and room departure revoke URLs and active transfers.
Buffering viewers catch up to the host clock without pausing the room.
