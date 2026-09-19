# XR companion tracking and room reveal

Accepted direction, revised from interview answers on 2026-09-17.
Implemented/staged for Linux/Monado later on September 17; see
[implementation and testing](XR_COMPANION_TEST.md). The intended behavior below
remains the design contract; its original investigation notes retain their scope.
Prim 0.3.6 is published and the user accepted the Linux single-client batch.
Friend testing remains useful but no longer blocks this next experiment.

## Intended behavior

Keep the Prim room, voice and human avatar present while another game owns the
headset. Sample physical head/hands relative to playspace; never inherit the
aircraft/mech/game-world camera transform. The user can lift a virtual headset
to see the Prim room, then lower it to see the game again. This is a view of the
virtual room, not camera passthrough or access to the game's eye textures.

Use one **Enable VR** toggle beside runtime activity. On the initial Monado
backend, enabling VR creates one persistent overlay session. That session supplies
tracking and can render the full room; it remains an overlay even when Prim is
the only environment visible. There is no separate tracking toggle and no normal
scene/overlay session recreation when a game starts or exits. Internally, separate
VR intent, session health, tracking validity, presentation and input policy.

| Prim state | Avatar and spatial listener | Headset output | Prim controller actions |
| --- | --- | --- | --- |
| VR disabled | Desktop head/pose | None | None |
| VR enabled, no other main app | Physical head/hands at room anchor | Full room from overlay session | Normal menu, movement, mute |
| Game + tracking | Same physical poses | Zero overlay layers | Gesture detection only |
| Game + quick peek | Same physical poses | Room reveal follows held controller lift | Gesture detection only |
| Game + latched reveal | Same physical poses | Full room covering game | Gesture detection only |
| Tracking unavailable | Explicit invalid/stale tracking; retain room membership | Hide overlay if view poses invalid | Cancel gesture |

While another scene app exists, hide Prim's headset menu/laser/pointer and gate
all ordinary controller actions, including locomotion, snap turn and mic toggle.
Do not merely hide their geometry. Showing the room does not grant permission
to activate those controls. Keep the existing mute and spatial voice behavior.

The focused Prim window stays usable with VR enabled, including while a game
owns the headset. Tab opens a **2D, mouse-only desktop menu**, with visibility
independent of the headset menu; this never creates a panel or laser in the
game/reveal view. WASD translates the Prim playspace anchor on the room floor,
without changing physical head/hand offsets. Accept movement only with window
focus and outside text entry; retain normal menu movement gating initially.
Mouse look must not drive the transmitted head or the audio listener.

Implementation default: a head-following mono desktop view and head-yaw-relative
WASD. Hidden XR submission must not freeze or black out the desktop room: it
needs its own render path when no headset frame is drawn. Reuse rendered output
where practical when XR is visible. Measure desktop rendering cost separately
from hidden XR cost; throttle an unfocused/minimized preview if necessary.
An independent observer camera can be a later option. User follow-up: consider a
lower-rate unfocused desktop preview, but keep the current rate for this first
build. Do not use a global frame cap that slows tracking/gestures; a future preview
throttle must preserve voice callbacks and libmpv decoding/background music.

## Evidence and backend choice

Core OpenXR reports the application's own state; it is not a portable foreign
scene-app inventory. An overlay's FOCUSED state is especially unsuitable for
deciding whether Prim may accept ordinary controller input.

- [XR_MND_headless](https://raw.githubusercontent.com/KhronosGroup/OpenXR-Docs/main/specification/sources/chapters/extensions/mnd/mnd_headless.adoc)
  allows an input/tracking session without graphics bindings. Headless operation
  and concurrent controller availability still need a real game proof.
- [XR_EXTX_overlay](https://raw.githubusercontent.com/KhronosGroup/OpenXR-Docs/main/specification/sources/chapters/extensions/extx/extx_overlay.adoc)
  supports a separate overlay session and zero-layer hidden frames. It provides
  main-session visibility events, not a game name or a reliable process-exit
  signal. Input reads do not consume input from the game.
- [Khronos runtime inventory](https://github.khronos.org/OpenXR-Inventory/runtime_extension_support.html)
  currently lists headless for Monado and SteamVR, and overlay for Monado but
  not SteamVR. Installed extension enumeration and actual coexistence determine
  support; inventory is not a runtime test.
- The libmonado API (see Prim's [optional client](../native/src/xr_monitor.rs))
  exposes client names and primary/active/visible/focused/overlay flags. Prefer
  its versioned C ABI to implementing raw Monado IPC. Load it optionally.
- An earlier standalone Godot spike
  recorded concurrent Monado sessions, stereo submission and active Knuckles
  tracking. It did not establish perceived transparency, physical button
  behavior beside a game, or sustained performance. Reuse its extension wrapper
  and small projection-alpha patch, not its old engine binary. The subsequent
  Prim implementation/evidence is in [companion testing](XR_COMPANION_TEST.md).
- A later SteamVR status adapter can inspect Valve's
  [OpenVR application API](https://github.com/ValveSoftware/openvr/blob/master/headers/openvr.h)
  (`GetCurrentSceneProcessId`). SteamVR rendering/overlay work is a separate
  backend decision. Running SteamVR alongside Monado is not a first milestone.

Selected backend: qualify a persistent OpenXR overlay on Monado first. Defer the
true-headless probe; it is a possible later backend for runtimes without overlays,
not a prerequisite for this implementation. Hidden means skip headset scene
drawing and submit zero layers, not continuously render a transparent room.
Preserve the frame/event/action loop, pose prediction and independent desktop/
audio/network processing. Measure hidden cost before accepting the implementation.

Use Godot's ordinary stereo projection/swapchain path for the visible overlay.
This avoids introducing a custom eye-copy/projective-overlay path. It is not a
frame-timing guarantee: EXT overlay explicitly allows predicted display times
to differ from the main app. Qualify motion, stereo and frame cost in-headset.
There is no current product requirement for Prim to become a technical main
session on Monado. Keep existing ordinary VR available on other runtimes while
their companion capability is unqualified; report unsupported companion mode
rather than silently creating a scene session alongside someone else's game.

Godot does not currently expose Prim's required headless/hidden-overlay policy.
Allow a focused engine hook for render/submission suppression and reuse the
existing graceful exit/drain behavior. Do not fake runtime shouldRender state
or change session graphics/overlay role in place. Enable/Disable VR still uses
the proven initialization and graceful teardown path. Any later backend switch
that requires session recreation must expose invalid poses during that gap while
the social session remains alive.

### WayVR coexistence gate

Ignore other overlays, including WayVR, when deciding whether another **main**
app owns the headset. WayVR's current OpenXR source submits its skybox and panels
in the same overlay session (placement 5) and shows the skybox when no main
session is visible. Prim remaining an overlay will not itself hide that skybox.
With ordinary layer ordering, placing Prim below WayVR can leave its skybox
covering Prim; placing Prim above can cover the dashboard too. A session priority
alone cannot place Prim between those layers from one WayVR session.

Desired result: Prim supplies the room while WayVR's dashboard remains usable.
Implemented a small WayVR background-owner allowlist patch, documented in
[build-support/wayvr](../build-support/wayvr/README.md). Prim is below WayVR;
WayVR suppresses only its skybox while Prim's overlay is active. The live ownership
transitions passed; final headset composition is still a human gate. Do not silently change its persistent configuration or introduce a dummy
main session merely to change WayVR's visibility event. The relevant source
context and patch are retained in [the WayVR notes](../build-support/wayvr/README.md).

## Runtime status and transitions

Report separate facts: selected runtime, reachability, main-app activity,
companion capabilities and current Prim mode. Present compactly, for example
`Monado · Other app: VTOL · Tracking` or `Monado · Prim room`.

Use `Prim`, `Other app`, `Idle`, `Unavailable`, and `Activity unknown` internally.
Do not equate failed IPC, an unset visibility event, a minimized game, a dashboard,
or missing headset tracking with Idle. Do not assume Monado just because the OS
is Linux. A configured runtime path does not prove the service/HMD is usable.
Use authoritative Monado client state when available and OpenXR events as
additional evidence; retain Unknown on other backends when evidence is absent.

Observe at a low rate, e.g. 2 Hz, outside frame-critical code; copy client-name
strings before the next API call. Do not initialize a scene session or restart
a VR service merely to refresh status. Account for socket-activation side
effects when deciding whether a Monado probe is passive. Do not request primary
or focused status from libmonado in the initial implementation.

Settled transition/input policies:

- Enable VR is the single explicit opt-in. Tracking is active whenever that XR
  session is healthy; visibility is independent. Remember intent separately from
  tracking validity. Disabling VR ends XR participation and restores desktop pose.
- With VR enabled, another game starting hides Prim's room and disables its
  controller actions. Require the gesture to reveal it again.
- On confirmed game exit, automatically reveal Prim and enable normal controls.
  A transient visibility/focus loss is insufficient. Other overlays do not prevent
  automatic return; hide/show never requires recreating the overlay session.
- Explicit Disable VR tears down Prim's XR participation and cancels automatic
  return, avoiding a loop where idle detection immediately re-enters VR.
- If state becomes unknown while a game may still exist, keep ordinary Prim
  controller actions disabled. A desktop stop/hide control remains available.
- A runtime disconnect returns to desktop without losing the room. Clear gesture
  state and stale poses; do not repeatedly recreate sessions against a failing
  runtime. Follow existing explicit-retry behavior initially.

Distinguish the user-facing Prim room state from the technical compositor role:
when alone, Prim remains an overlay session while providing the full room UI.
Prove this on Monado rather than claiming it became the primary scene client.
On return to full Prim interaction, require held buttons/sticks to return to
neutral before accepting actions; a grip/trigger held in the game must not
immediately click a menu or toggle mute. In the other direction, use main-session
events to disable actions promptly rather than waiting only for status polling.

## Coordinate and gesture policy

One transform maps runtime playspace into the Prim room:
`room_pose = room_from_playspace * physical_pose`.
Initialize it so the first valid physical head matches the user's existing Prim
location/yaw. WASD translates that transform; physical motion remains relative
to it. Avatar targets and Steam Audio listener use the same transformed head.
Separate render cameras from that listener/pose source in `main.gd`.

Prefer stage/floor coordinates, with an explicit fallback when unavailable.
On recenter/reference-space changes, compensate the anchor to preserve room
continuity, or re-anchor from the last valid head and reset IK once. On tracking
loss, invalidate the relevant head/hand bits; never silently substitute moving
desktop poses. Debounce restoration and clear pending gesture state.

First gesture implementation should be a deterministic state machine with
recordable pose/button inputs, not an animation that determines behavior:

1. Either tracked hand remains in a generous head-relative front zone for about
   500 ms, with grip initially released. Tune a provisional 15–40 cm reach and
   tolerance for controller orientation in-headset. No finger tracking required.
2. A fresh grip press arms lifting for that hand. Require upward physical motion
   relative to the head, approximately 15–25 cm within a bounded interval. Use
   gravity/floor up rather than head pitch to define lift direction.
3. While grip remains held, map lift above the armed position to a clamped reveal
   fraction, with hysteresis/smoothing. Keep the camera physically head-tracked;
   animate the reveal/mask, not eye position. A completed lift shows the full room.
4. **Quick peek:** keep holding, lower back near headset level, then release;
   the room follows the lowering and ends hidden. **Latch:** release while fully
   raised; the room stays visible. Initial edge-case default: release partway
   through an opening lift settles hidden, so only a completed lift can latch.
5. From a latched reveal, either hand slightly above the head can freshly grip,
   pull down and release near headset level to hide. A partial lowering released
   early settles back to the latched room. No second dwell required for closing.
   Add separate thresholds, release-to-rearm and cooldown; one hand owns each
   gesture, and the other cannot begin a competing gesture until it ends.

Cancel an incomplete gesture on pose loss, excessive delay or role change, back
to its prior stable state. The held quick peek itself has no short timeout once
the lift succeeds; keep showing it while grip and poses remain valid. A completed
reveal remains latched until lowering, explicit desktop hide, or invalid HMD
pose/runtime loss. Either hand or the desktop escape path can close it. Game
start/exit overrides gesture state; clear the latch and use the transition policy.

No game input suppression in the first version: the user explicitly accepts
shared controller input, including grip, reaching the game. Do not call Monado's
input-blocking API. Automatic pause, input blocking and game-audio ducking are
separate policies, not implied by the reveal.

## Implementation sequence and gates

1. **Separate state and add monitoring.** Add runtime observation and capability
   reporting beside Enable VR. Split the existing `xr` boolean's jobs into
   tracking source, presentation and permitted input. Separate desktop menu
   visibility from the headset panel. Preserve 0.3.6 desktop/VR switching and
   window-size/input recovery. Unit-test state/input policies without a runtime.
2. **Prove persistent overlay coexistence.** Reuse the existing overlay scaffold
   on Monado, then one actual VTOL/mech-vr session. No true-headless probe first.
   Compare head/both-hand validity and grip input, scene activity before/after,
   no headset image, game input and clean attach/detach in both launch orders.
   Also verify overlay-only Prim with no game, and the WayVR background/dashboard
   ordering above. If it fails, stop at the concrete incompatibility before
   selecting a different backend. Do not stop/restart Monado or inject runtime/GPU
   failure into the user's game session.
3. **Integrate tracked presence and handoff.** Wire the single toggle, avatar,
   listener, anchor, independent desktop room/menu and focused-desktop WASD.
   Automatically hide on game start and return on confirmed game exit without
   recreating XR. A second local Prim client can observe the player
   until friends are available. Verify physical turning changes room audio,
   game movement does not move the room, and tracking/recenter recovery is sane.
4. **Add visible overlay, then gesture.** Start with a development-only reveal
   control so compositor correctness and tracking alignment can be inspected
   independently of recognition. Verify full stereo and hidden zero-layer frames;
   do not submit cross-session depth that could occlude the room against the game.
   Then implement held quick peek, release-to-latch and lower-to-close with
   recorded pose/button traces for false starts, hand loss and partial releases.
   Ordinary Prim controller actions stay gated through all game/reveal transitions.
5. **Qualify the handoff.** Exercise game launch/exit, hide/show repeatedly, hand
   loss, HMD removal, WayVR/dashboard interaction and unknown runtime state.
   Compare the game's frame time with Prim stopped, hidden and revealed. Re-run
   desktop/VR return sizing/input checks. Follow with one remote observer,
   OBS mirror and spatial voice; leave Windows/SteamVR/WiVRn marked unverified
   until each is actually tested.

The first work block should target steps 1–2 and finish with a headset gate.
The first useful Prim build adds step 3: tracked presence, desktop controls and
automatic handoff, before gesture polish. Steps 3–4 follow only after coexistence
works. Do not make gesture polish, SteamVR-on-Linux setup, capture automation,
host migration or another game adapter prerequisites for the first useful
tracked spectator session.

Likely edit boundaries: `project/xr/session_lifecycle.gd`, new focused runtime
monitor/gesture modules under `project/xr`, pose/listener/input separation in
`project/main.gd`, and `project/ui/world_menu.gd`. Native extension/engine changes
own OpenXR session chains and frame submission; GDScript owns product state and
gesture policy. No new network pose protocol is expected for head/hands.

## Tunable defaults, not blockers

The seven interview topics are settled above. Tune dwell/zone/height thresholds,
reveal animation and partial-release feel using the headset. Start with a
head-following desktop view and head-yaw-relative WASD; explicit runtime retry
after failure remains the existing default. No extra permission round is needed
for routine implementation choices within this plan. WayVR's ownership transitions
have live evidence; actual headset composition, VTOL input and gesture feel remain
human gates.


September 18: see [SteamVR fallback sequencing](STEAMVR_COMPANION_PLAN.md) for
the next backend, verified installed extensions, OpenVR background tracking
probe, interview decisions and Windows friends qualification.
