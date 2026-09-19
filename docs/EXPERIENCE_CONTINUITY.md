# Prim living room and external experiences

Design context agreed with the user on 2026-09-16. This records direction and
candidate experiments, not an implementation commitment or a change to the
current project priorities. Existing playback/avatar qualification and mech
control/contact work can proceed first. Nothing described here is implemented
unless identified as existing evidence below.

September 17 update: the user selected desktop/OpenXR scene-session lifetime
for implementation before afternoon media qualification. The Linux checkout now
has a tested candidate; see [XR lifecycle](XR_LIFECYCLE.md). Invisible tracking
while another game owns XR, visible overlay and game IPC remain separate proofs.

Discussion task: `01a0ab5e-d648-7aa1-a37c-f04038a8d1f0`.

Contributor orientation: [project direction](DIRECTION.md) summarizes this plan
alongside the other tracks. Exploratory draft implementations are welcome under
[CONTRIBUTING.md](../CONTRIBUTING.md). The dated evidence below is historical;
use the linked lifecycle/companion docs for subsequently implemented behavior.

## Product intent

Prim is a shared virtual living room. People retain a social presence through
voice, avatar pose/IK and visemes, and screens showing media or a friend's game.
Point-cloud/RGBD spectator views may extend those screens later. The room can
become invisible, voice-only, or locally quiet while someone plays, like an
actual living room receding when they put on a VR headset. They can eventually
"virtually flip up the headset" to look back through an overlay.

Prim can also supply a Mumble Link-like voice integration and a Steam-like
download, launch, and join experience. A trusted external executable should feel
about as straightforward to enter as a world, despite having a different
process and deployment lifecycle. It can use Prim's voice/context integration,
use a lobby ticket to join its own multiplayer backend, or adopt more of a
future Prim SDK. Prim does not need to own its gameplay simulation.

The initial publisher is the Prim developer; the initial integrated game is
mech-vr. Supporting arbitrary third-party games and publishers is later work.
The first useful case does not require multiplayer mech combat:

1. A Prim tester accepts an offered mech-vr build and launches a solo session.
2. Friends remain in Prim and keep talking to the tester.
3. They see the tester's roughly reconstructed physical head/hand movement and
   human avatar, while watching the game's mirror on the room screen.
4. The tester exits, fails to launch, or crashes; the social session remains
   available and they can return or retry without gathering everyone again.

This is the remote equivalent of watching someone try a VR game in your actual
living room. A visible Prim overlay improves it but is not a prerequisite for
the desktop spectator and voice version.

## Agreed direction versus open design

Agreed:

- Keep the living room, its avatars/visemes, and shared viewing surfaces as
  first-class product features; launching games is an extension of that room.
- Keep party membership alive across activities. Choose voice presentation
  according to the activity, rather than muting the whole party on launch.
- Aim for desktop startup and explicit entry into the headset when available;
  leave XR while retaining the process, desktop window, and social session.
- Accept trusted native executable execution. A browser/VRChat-style sandbox is
  not required for the initial developer-published experience.
- Provide a small SDK/IPC integration for lobby setup, identity mapping, and
  handoff. Start with mech-vr and keep solo spectating useful before multiplayer.

Open: exact process placement, IPC transport/API, renderer extraction, headset
backend order, stream encoding/transport, supported voice profiles, installation
retention, and the later publisher trust scheme. Proposed defaults below remain
subject to bounded experiments. This note authorizes no implementation or release.

## Independent lifetimes and possible process boundaries

Separate these responsibilities conceptually before choosing a deployment:

| Responsibility | State it owns |
| --- | --- |
| Party/session | Membership, peer identity, room control, voice transport, activity presence |
| Voice and tracking | Capture/mute, playout, routing, tracking-source selection, viseme input |
| Prim presentation | Living-room rendering, avatar IK, screens, desktop/scene/overlay UI |
| Activity integration | Launch instance, game readiness, player mapping, join ticket, pose/audio/media capabilities |
| Package/launch manager | Release verification, installation, process supervision, retry and rollback policy |
| Game | Gameplay, authority, physics, its own world coordinates and networking |

A first version may simply keep Prim's Godot process alive beside the game.
Extract networking/VoIP into a resident service only when overhead or recovery
evidence warrants it. If extracted, voice playout as well as packet receipt must
survive renderer stalls; moving only sockets does not establish audio continuity.
The launcher is a plausible supervisor, not automatically the right audio host.

Measure background Prim CPU/GPU/VRAM and audio timing while mech-vr is loaded.
Try bounded rendering, stopping hidden screen decoding, and suspending unnecessary
local visual work before assuming a full service rewrite is necessary. Remote
observers must still receive the tester's presence even if their local room
renderer sleeps. A detached renderer needs a state snapshot when it resumes.

Ordinary game failure should be isolated from the party. GPU/runtime/OS failure
can still affect several processes. Game-host, media-provider, and room-host loss
are distinct cases; current Prim room-host loss disconnects the room. Do not
promise host migration merely because game launch survives a child-process exit.

## Desktop, immersive scene, and overlay presentation

Target user flow: start Prim on the desktop, join friends, then choose to display
it in the headset when a runtime/headset is available. Later leave its XR session
to give a game the headset while preserving Prim's desktop window and connection.
Scene and overlay roles can be separate presentation modes.

Do not assume that toggling `Viewport.use_xr` releases a session or that
`XRInterface.initialize()` alone makes late entry work. Audit the pinned Godot
engine's early OpenXR/Vulkan initialization, device selection, session and
swapchain destruction, and recreation. Retain the possibility of a narrow engine
patch. Dynamic entry/exit is a hypothesis to prove, not a verified current feature.

Test desktop start without a running runtime, later runtime availability, repeated
entry/exit, another game's XR ownership, recentering, tracking recovery, audio
listener migration, and failure while attaching. Preserve room identity, mute,
settings, avatars, and media state. Avoid unnecessary changes to shared VR services.

Linux/Monado has the separate `godot-openxr-overlay` experiment. Windows/SteamVR
needs an OpenVR-compatible overlay path; do not assume the Monado extension is
portable to it. Validate physical input and perceived rendering on each backend.
Switching a session between scene and overlay may require recreating it.

The desktop-alive fallback is independently worthwhile and can precede overlays.
If same-process XR re-entry proves expensive or unreliable, consider a separate
presentation process while preserving the user's session; document the tradeoff.

## Voice follows the activity

| Activity | Desired conversation |
| --- | --- |
| Ordinary room | Spatial living-room voices |
| Flat game or solo mech test | Continuing living-room/party conversation |
| Shared VR multiplayer such as mech-vr | Those players heard through game-space microphones, cockpit intercom, or radio; no duplicate room rendering |
| Loading, failed launch, or game exit | Party conversation stays available; restore the selected fallback on failure |
| Look back into Prim | Bring room conversation into focus; exact game/room receive and transmit transition remains to be tested |

Route per participant and activity instance, not by executable name alone. Two
copies of the same game may be in different sessions. A friend remaining in Prim
must remain reachable; background room voices versus an explicit call/attention
interaction is still open. Do not treat launching a game as consent to transmit
into an unrelated server or change a deliberate mute.

Start with game-supplied context/poses/radio parameters and Prim-owned rendering,
following the Mumble Link pattern. Later, allow a game to consume individual
decoded voice streams when it needs its own mixer or occlusion. Use an explicit
handoff, readiness, and heartbeat/expiry so stale IPC cannot leave voice silent or
produce duplicate playback. Keep capture/mute and the party fallback in Prim.

User mute/block/volume choices remain separate from game rules such as proximity,
radio channels, or teams. A persistent out-of-game party is not an enforceable
competitive-game secrecy boundary. Visemes should follow the relevant existing
voice source, without adding a second microphone capture or voice connection.

## Seeing the tester and the game

When Prim relinquishes XR, it must not assume it can still obtain live headset
and controller poses from its own inactive session. For mech-vr, export the
tracked human head and wrists, optional fingers, timestamps, validity, and a
tracking/recenter epoch through the local integration.

Keep the physical tracking frame distinct from the moving cockpit, robot body,
game world, and living-room placement. Map the human-scale tracking frame to the
tester's chosen room anchor. Flying the mech a kilometer or rolling it must not
launch or rotate their Prim avatar across the room. Recalibration/recentering
needs an explicit discontinuity; stale tracking should visibly degrade rather
than freezing someone in a misleading pose. Prim can solve human avatar IK from
these targets; the mech's constrained robot arms are a different representation.

First spectator output is an ordinary 2D mirror with game audio, on the existing
room screen, with one selected source. OBS-first live ingest remains an acceptable
proof path from the media roadmap; an integrated mirror producer can follow.
Avoid capturing/rebroadcasting Prim's received voices into the game stream.
Keep game audio separately controllable and measure its alignment with visible
hands and action. A live source must not inherit inappropriate VOD seek/catch-up
behavior. Stop/replacement/game-exit should retire its publication cleanly.

Video bytes need separate bounded transfer/encode queues and upload limits;
separate streams alone do not remove shared-link congestion. Keep voice and pose
responsive under load. RGBD and semantic spectator exports remain later extensions
with their own capture, coverage, coordinates, and performance gates. No need for
eye/depth texture interception to prove this first authored-game mirror.

## Launch and SDK contract to explore

Start with a small, versioned local contract, not a general engine plugin system:

- Session-scoped participant identity mapping and an optional game join ticket.
- Activity instance and release identity; preparing, launched, ready, failed,
  exited, and return states. A started process is not a ready game.
- Explicit capabilities for activity state, tracking, voice context/rendering,
  and spectator publication; request only what the integration uses.
- A local endpoint locator and short-lived launch capability. Do not pass the
  room's long-lived shared secret to the game or interpolate peer data into a shell.
- Timeout, cancellation, late reply rejection, and reconnect/retry behavior.

Game networking can remain independent. Prim can supply lobby rendezvous and
player mapping; sharing bounded application transport later is optional and does
not solve mech authority, prediction, or reconciliation. Preserve standalone
mech launch and make missing Prim integration a supported state.

## Distribution and trust

Initially the user accepts normal application execution for builds published by
the Prim developer. Arbitrary GDScript PCK loading has comparable code-execution
trust implications; that fact is understood. Packaging, process isolation, and
sandboxing are independent. A versioned Godot runner plus PCK in its own process
may be useful without imposing a shared-engine ABI on every game.

Reuse the launcher's signed metadata/verification experience. An offered release
should identify publisher, immutable manifest, supported platform artifacts and
their hashes, protocol compatibility, and launch entry point. Linux and Windows
friends can use different artifacts under the same release identity. Verify the
complete runnable package before launch, safely stage/extract it, preserve saves
outside immutable versions, and distinguish download, install, and launch state.
Keep failed downloads or missing platform builds from tearing down the party.

The current movie range service and temporary RAM cache are not a complete
executable installer. Reusable transfer primitives are useful, but runnable
packages need their own retention and verification lifecycle. HTTPS distribution
is a valid first step; direct friend transfer or `iroh-blobs` can follow a need.
User-controlled installation/launch should remain explicit.

Later, Sigstore/provenance can establish expected publisher/build identity and
auditable release history. A matching digest means identical bytes; a signature
does not mean harmless code. Friends reporting use of that release is useful
social evidence, not remote execution attestation. No broad sandbox or public
publisher/reputation platform is a prerequisite for the initial developer builds.

A separate future scene viewer could accept glTF plus deliberately constrained
Lua/Udon-like scripting, sharing engine/native libraries efficiently. Its exposed
API and resource limits would define the restriction; embedding Lua by itself
does not establish one. Keep this option open without making it a prerequisite
or forcing trusted native games into that format.

## Candidate tactical sequence, when this work is selected

These are dependency-ordered experiments within this track, not the next items
on the overall board. Each stops for evidence before expanding scope.

| Step | Prim work | Mech work | Evidence gate |
| --- | --- | --- | --- |
| A. Presentation lifecycle | Audit/prove desktop start and XR attach/detach while party/voice survives | Remain a separately launched XR app | Repeated headset ownership transitions, no room rejoin or unexpected unmute; assess both target runtimes separately |
| B. Local activity handoff | Minimal IPC/readiness and background cost measurement | Optional adapter and normal standalone fallback | Launch, cancel, stall, crash, retry and return preserve party; quantify CPU/GPU/VRAM/audio cost |
| C. Living-room playtest | Accept external tracking for room avatar; qualify one live screen | Publish physical head/hand tracking and a mirror, retaining solo gameplay | One tester and one observer can talk, interpret hand motion, watch, recenter and return; no echo/robot-world pose leakage |
| D. Tester distribution | Developer-signed manifest, platform selection, install/launch UX | Produce and verify installable Linux/Windows packages | Clean-machine install, corrupt/interrupted transfer, cached launch, incompatible build and game crash remain recoverable |
| E. Richer presence | Overlay reveal/input handling; profile-based voice handoff | Supply game voice context/radio behavior as needed | No duplicate speakers or leaked control input; game/room transitions make sense to real participants |
| F. Conditional extraction | Move session/VoIP beyond renderer only if B/C show a need | Keep the adapter stable | Renderer suspension/restart preserves voice, identity and bounded state; measure improvement |

Real multiplayer mech work is separate from A-D. When selected, a tiny shared
session/pose proof can validate lobby mapping before networked physical combat.
Host migration, generalized UGC, arbitrary third-party integrations, remote game
input, and RGBD delivery remain separate decisions. If a gate fails, retain the
useful narrower mode rather than treating the whole direction as blocked.

## Evidence and related work

Inspection on 2026-09-16, not a fresh runtime test:

- Prim `main` at `015ce98`, with existing uncommitted playback/sharing/runtime-VRM
  work. `PrimSession` remains a Godot node; exiting drops its handle. Voice capture
  workers do not make the whole session independent of the Godot process.
  See [protocol](PROTOCOL.md), [session](../native/src/session.rs), and
  [startup](../project/main.gd). At that revision, startup selected desktop/XR once;
  later transition work is described in [XR lifecycle](XR_LIFECYCLE.md).
- Launcher already verifies signed metadata and supervises Prim's process:
  [launcher README](../launcher/README.md). It is not a general experience launcher.
- mech-vr `codex/melee-greybox` at `52de362` has local control/contact/replay and
  latency simulation, not real multiplayer or Prim IPC. Its current physical
  test gate remains independent of this plan.
- An earlier separate Monado spike recorded coexistence/submission and orderly
  shutdown; it did not prove physical input or perceived rendering. Prim's later
  in-repository evidence is in [companion testing](XR_COMPANION_TEST.md) and
  [companion follow-up](COMPANION_NEXT_PLAN.md).
- [Media roadmap](DIRECT_MEDIA_PLAN.md), [beta follow-up](BETA_PLAYBACK_FOLLOWUP_PLAN.md),
  [avatar boundaries](AVATARS.md). The Prim-side requirements for any external
  game are recorded above; no separate game's private notes are prerequisites.
- Before a richer RGBD spectator integration, independently demonstrate stable
  capture/delivery, correct coordinates and viewpoint behavior, bounded resource
  use, and acceptable frame timing. A working 2D mirror or an unrelated flight
  demo does not establish those properties. No RGBD integration is claimed here.

Reference patterns discussed: [Mumble Link](https://www.mumble.info/documentation/developer/positional-audio/link-plugin/),
[Steam lobby integration](https://partner.steamgames.com/doc/api/ISteamMatchmaking),
[Godot pack security](https://docs.godotengine.org/en/stable/tutorials/export/exporting_pcks.html),
[OpenXR runtime extension inventory](https://github.khronos.org/OpenXR-Inventory/runtime_extension_support.html),
[OpenVR overlays](https://github.com/ValveSoftware/openvr/wiki/IVROverlay_Overview),
and [Sigstore verification](https://docs.sigstore.dev/cosign/verifying/verify/).
