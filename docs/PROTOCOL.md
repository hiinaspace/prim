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

The authenticated connection hello is version 2; older builds must be updated.
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
avatars and preserves local movie playback. Reconnecting can create a new room.
No seamless host migration or ongoing split-room convergence is implemented;
simultaneous first joins can produce separate rooms. For the friend test, let one
person connect first and wait for lobby publication before others connect.

Bounded channels and coalesced pose updates prevent an unbounded slow-consumer
backlog. Losing critical native events fails the session visibly rather than
silently keeping an inconsistent membership state. This protocol intentionally
has no general Godot MultiplayerAPI adapter, media transfer, queue or UGC layer.
