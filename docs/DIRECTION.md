# Project direction and architecture boundaries

This is the short orientation for contributors. It distinguishes today's code,
the intended direction, and decisions the maintainer will review before adoption.
The contribution policy is in [CONTRIBUTING.md](../CONTRIBUTING.md) and
[AGENTS.md](../AGENTS.md). The longer plans below contain rationale and dated
experiments; they are not an ordered backlog. Exploratory draft implementations
are welcome, including in unsettled areas; explain departures from the direction
and keep them easy to revise rather than silently turning them into dependencies.

## The purpose

Prim is a shared virtual living room. A small group can talk, inhabit expressive
avatars, watch something together, or watch a friend play. Over time that social
presence should survive changes of activity and presentation: desktop, immersive
room, a glance back from another VR game, and return to the room.

Familiar social-VR features such as FBT, readable menus and nameplates, and good
controller/runtime support help that experience now. VRChat parity is useful
where it improves being together; it is not the full architectural specification.
Longer-term interests include user-supplied avatars/worlds and dynamic editable,
saveable room content in the spirit of Resonite. Those are design tracks, not
existing capabilities or a mandate to reproduce another application's internals.

## What exists now

- A Godot room with a Rust native session, a six-person authenticated Iroh mesh,
  shared-secret DHT discovery, spatial voice, tracked avatars and synchronized media.
- One room host orders playback; a different participant can provide a shared
  local file. Media uses dedicated authenticated range transfers and bounded
  temporary caching. URLs are loaded by each viewer; live inputs have different
  playback semantics from seekable movies.
- Bundled VRM avatars with head/hand tracking, local IK, fingers and visemes.
  Runtime local-file VRM import is an offline proof, not peer avatar distribution.
- Desktop/XR lifecycle and companion paths. OpenVR stereo peek is experimental;
  platform/controller qualification is incomplete. See [XR lifecycle](XR_LIFECYCLE.md),
  [companion follow-up](COMPANION_NEXT_PLAN.md), and
  [OpenVR implementation notes](../build-support/openvr-overlay/README.md).
- A launcher that verifies updates and supervises the application. It is not yet
  a general game launcher or renderer-independent party/voice service.

Room-host loss still ends the session. Persistent room restoration, partition
merge, general UGC, world editing/saving and external-game SDK integration are
future work. See [protocol](PROTOCOL.md) for the current wire and failure behavior.

## Design tracks coordinated by the maintainer

### Identity, P2P lobbies and room continuity

Keep room identity, participant identity, live connection/incarnation, playback
coordinator, and file provider distinct. Discovery provides rendezvous; admission
requires authentication. A file provider does not acquire room authority.

The continuity direction is a room that outlives its current coordinator, can be
restored from participants' cached state, and stays usable on both sides of a
partition before converging. The current trusted-friends secret/mesh is a starting
point, not a complete public identity, admission or moderation system. Avoid
introducing accounts, permanent owner privileges, a central lobby service, or a
new transport as an incidental feature dependency.

[Room continuity](ROOM_CONTINUITY_PLAN.md) records earlier product decisions and
candidate mechanics, including moderation succession and partition behavior.
Those mechanics still need a selected implementation slice and validation. The
initial user-facing testing target is one shared friends-scale lobby distributed
with the maintainer's signed build. It is not a private-membership guarantee.
Private rooms and different identity/lobby UX remain open near-term work.

### Durable, editable content and UGC

Future avatar/world loading must fit asset identity, transfer, caching, resource
limits, trust and compatibility. An offline VRM preview does not settle these
contracts. Trusted external executables and restricted user-created world content
may have different loading/execution models; process isolation alone is not a
sandbox. The general world-content and scripting contract remains open.

The persistence direction is replicated durable room/object state with stable
identities and recoverable edit history. Keep that separate from transient pose,
voice, presence, clock samples, election state, transfers and provider availability.
The existing room plan proposes a bounded Automerge experiment beginning with
coherent playback intent; it does not specify a complete Resonite-like object
model. Do not turn live Godot nodes or
every tracking update into the saved network schema as a shortcut.

See [avatar boundaries](AVATARS.md) and [room continuity](ROOM_CONTINUITY_PLAN.md).

### Media and streaming

Keep media transport, decode/rendering, room playback intent, and provider ownership
separate. Preserve bounded memory/queues, consent for relayed file traffic, source
replacement/revocation and responsive voice/pose. Reusing Iroh does not by itself
settle a new streaming protocol or bandwidth policy.

The useful near-term scenario is friends watching someone's gameplay with voice
and visible avatar motion. Existing live URL playback and an OBS-based capture
experiment can establish that experience. Integrated capture, distribution,
encoding, remote input and RGBD each have separate design and validation work.
See [media plan](DIRECT_MEDIA_PLAN.md), [live-input testing](LIVESTREAM_TEST.md),
and [capture research](LIVESTREAM_CAPTURE_RESEARCH.md).

### Session lifetime, processes and external activities

Treat party/session, voice/tracking, room presentation, activity integration,
package supervision and game simulation as separate responsibilities. Their final
process placement is open. Current native networking workers and OpenVR helpers
do not make the whole party independent of the Godot process.

The target is continuity of identity, voice and tracked presence while a game
starts, runs, crashes or exits. Measure background overhead and recovery first;
extract a resident session/voice service when the evidence supports the tradeoff.
Media playback after closing the renderer is not required for the first resident
proof. Voice policy follows the activity and participant: keep solo-game party
conversation, avoid duplicate voice when a shared game supplies its own spatial
context, and preserve explicit mute choices.

Physical human tracking, game-world motion and Prim room placement are different
coordinate frames. A vehicle moving in a game must not fling the player's human
avatar around the room. Start with one trusted integration and a useful solo
spectator case before generalizing an SDK. The rationale and candidate experiments
are self-contained in [experience continuity](EXPERIENCE_CONTINUITY.md).

## Reading plans without inheriting accidental constraints

Current behavior comes from code plus appropriate tests. This page summarizes
direction; detailed plans explain it. Dated source hashes and test reports are
historical evidence. If two documents disagree on a design affecting your task,
identify the discrepancy and explain an experiment's chosen assumption rather
than silently treating it as resolved. Correct stale descriptions when the intended behavior
is already clear. No private tracker or off-repository checkout is required to
understand or propose a contribution.
