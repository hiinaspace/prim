# Companion follow-up after the 0.3.8 friends session

Updated 2026-09-19. The user subsequently authorized implementation of steps
1–4. Step 5 remains deferred; the source/build/public-repo audit belongs to a
separate task. Release 0.3.9 is being prepared at the user's request.

## Implementation and qualification

- Steps 1–3 are implemented: runtime-local libmonado selection, deliberate
  unknown-status reveal, persisted above-overlay compatibility ordering,
  tracked desktop yaw, neutral-gated peek locomotion, independent deafen, and
  identical headset-aimed wrist controls. Cooperative ordering remains default.
- Step 4 is an opt-in OpenVR stereo prototype. Linux SteamVR rendered correctly
  in the user's headset, including lift/hide and wrist dwell toggles. Both
  native Linux and Windows builds compile. Windows overlay/headset and other
  controller bindings still need testing; the old background-only mode remains.
- Wrist text ordering was corrected during the headset test. The final smaller,
  headset-facing wrist panels passed the rendered preview and the user's
  subsequent Monado headset/dwell check.
- Local checks: 77 real private-Monado companion checks (including placement
  1/10 and XR-only restart), 84 menu/yaw/deafen checks, 81 policy/gesture/dwell
  checks, three native status classification tests, and the dynamic-library
  fallback fixture pass. A rendering SteamVR reference-game run passed 25
  tracking/ownership/continuity checks. A later interactive run exposed a
  latched-gesture override of explicit Hide, now covered by the Monado test.
- See [Arch retry / agent handoff](ARCH_COMPANION_TEST.md) for the remaining
  friend's-machine visibility test and
  [OpenVR prototype details](../build-support/openvr-overlay/README.md).
- No foreground-game performance or latency claim follows from these results;
  compare those under actual game load before making the prototype a default.

## New evidence

User-reported friends testing of 0.3.8:
- RTSP playback worked.
- Prim continued alongside Pavlov, including the Monado overlay peek gesture.
- Windows SteamVR background tracking worked while a friend played VRChat.
- An Arch/Envision user had tracked poses but no visible Prim room, apparently
  seeing WayVR's background instead.

The most recent support ZIP in ~/Downloads contains a 0.3.8 session starting
2026-09-19 03:32:49 UTC. OpenXR initializes Monado v25.1.0-505-g3170a8855, while
its optional status client reports v25.1.0-294-ga3574442e and refuses IPC. Prim
then reports “Monado • Activity unknown • Tracking”. This is a demonstrated
status-observation failure, not evidence that the gesture itself failed.
The 0.3.8 session_lifecycle.gd reset reveal_fraction whenever activity != other,
so unknown activity suppresses explicit reveal as well as automatic display.
The ZIP's GLIBC_2.43 failures belong to older 0.3.6 sessions, not this 0.3.8 run.
No raw support logs or personal paths are copied into this plan.

There is also a separate known portability dependency: our local WayVR patch
suppresses its skybox while Prim is visible. It is not shipped by Prim's updater.
Prim placement 1 sits below WayVR placement 5. Prim cannot insert its session
between WayVR's skybox and panels, which WayVR submits together. The report does
not establish the friend's WayVR version or whether its skybox is an additional
obstruction after the status issue is fixed.

## Accepted behavior

- Desktop pointer capture and mouse yaw may reposition the Prim playspace while
  physical head/hand tracking remains active. Physical pitch/roll remain tracked.
- While peek is visibly active, allow controller-stick locomotion in Prim even
  though the foreground game also receives those inputs. Keep Prim laser,
  ordinary menu-button and face-button mute actions disabled beside another game.
- Identical wrist controls on both hands use headset-facing aim, not eye tracking.
- Microphone mute and deafen are independent. Deafen silences received Prim voices
  and media only; it does not change microphone transmission, other apps or room
  playback state. Undeafen restores local listening levels.
- Offer an explicit above-WayVR compatibility option; it may cover WayVR panels
  while Prim is visible. Cooperative WayVR background suppression remains the
  preferred composition when installed.
- Try full stereo OpenVR projective peek before judging it. If feel/performance
  is poor, evaluate a head-relative mono quad around 2 m away, optionally curved,
  with controller-attached wrist quads.
- First resident-process proof requires voice and tracked presence, not media
  audio after the Godot renderer closes. Media can stop locally for this proof.

## Sequence and gates

### 1. Arch/Envision visibility and diagnostics

Resolve the active runtime manifest including symlinks and relative library_path;
prefer libmonado from the selected runtime's installation prefix over an unrelated
system library. Preserve PRIM_LIBMONADO as an explicit override. Test candidates
by opening the client and observing successful IPC, not by library load alone.
Do not globally set IPC_IGNORE_VERSION: a matching public C API version does not
establish compatible private service IPC. Report selected library/runtime paths,
versions and failure reason in support diagnostics with existing path scrubbing.
Avoid continuously retrying and flooding logs on an unchanged mismatch.

Decouple explicit reveal permission from knowledge of scene ownership. A valid
OpenXR overlay and valid head/controllers can reveal explicitly even when activity
is unknown. Unknown must still forbid automatic scene takeover and keep normal
Prim scene controls disabled. Show a useful status/reveal control instead of
silently remaining invisible. Tracking loss cancels presentation normally.

Add a persisted composition setting: normal/cooperative placement, or above other
overlays compatibility. Placement is chosen at session creation; changing it uses
orderly XR restart while preserving room/voice. Do not pretend it is a live layer
reorder. Explain that compatibility ordering can cover WayVR panels. Consider
providing an explicit Reveal Prim button alongside the gesture for diagnosis.

Test: mismatched host libmonado versus selected runtime prefix; no usable libmonado;
unknown activity plus valid tracking; patched and unpatched WayVR; idle and active
game; repeated enable/disable and composition-setting restart. Friend checks the
same failing Arch setup before this is considered fixed there.

### 2. Locomotion and local audio controls

Separate permissions for tracked avatar, stick locomotion, menu/laser actions and
watch interaction. A single room_primary flag must no longer gate all input.

Desktop: click the focused scene to capture; Tab/Esc/menu/focus loss releases.
Mouse horizontal motion rotates the playspace about the current tracked head,
including remote pose and spatial-audio coordinates. No synthetic pitch/roll is
added during tracked mode. Do not steal the cursor from a running game. Preserve
ordinary desktop mouselook when VR is disabled.

Peek: enable locomotion only above a clear visibility threshold; require sticks
to return neutral on entry to avoid inheriting a held gameplay stick. Support
existing move/snap/smooth-turn preferences. Hide/lost tracking immediately stops
Prim locomotion. The other game continues receiving input. Keep room movement
bounds and head-centred turning unchanged.

Audio: explicit local receive buses for peer voices and movie audio, including
direct stereo. Deafen gates their final output without pausing the player,
disconnecting peers, changing shared volume or losing mute state. New peers and
output-device changes must honor it. Show deafen in the main menu. Proposed
initial lifetime: session-local, preserved across XR/provider changes, reset on
fresh launch; microphone retains its existing startup policy.

Test: yaw/head-pivot/remote pose continuity; mouse focus and menu transitions;
held stick on peek entry; quick peek and latched peek; no Prim menu/laser/mic button
activation in a game; all four mute/deafen combinations with two peers and media;
voice/media resume without backlog or time shift. Rerun relevant Monado and
SteamVR lifecycle tests after input changes, not only pure policy tests.

### 3. Wrist UI on the working Monado peek path

Reuse one audio-control state model for the main menu and two identical watches.
Attach to tracked controller/wrist transforms; no dependency on avatar mesh bones.
Only show/activate during peek initially. Two large targets, mic and listening,
with explicit states, visible dwell progress and a short completion haptic.
Start with approximately 650 ms dwell; tune in the headset. Require a fresh look
away before another activation, with a shared arbiter/cooldown across both hands.
Cancel dwell on tracking loss, hiding, excessive aim movement or target change.
Do not let the hand used for the reveal gesture accidentally toggle a target.
Make “deafened, microphone live” unmistakable. Test seated/standing, both hands,
quick/latched peek, intentional repeated toggles and accidental activation.

### 4. Bounded OpenVR peek experiment

Recloned the user-provided upstream into /home/s/code/godot-openvr-overlay,
commit c766e97 (https://github.com/hiinaspace/godot-openvr-overlay). Source inspection
confirms a Godot XRInterfaceExtension using VRApplication_Overlay, two Vulkan
projective eye overlays, direct left-eye array submission and a right-eye texture
copy, controller pose/actions and haptics. Use this as the first spike implementation;
no reason yet to add image transport to the background helper.

Concrete adaptation work before Prim integration:
- Replace VREvent_Quit -> SceneTree.quit with backend-disconnected notification
  and orderly detach; Prim's room must survive runtime shutdown.
- Package action manifests/bindings as real files; globalize_path(res://...) does
  not extract assets from the packed game. Existing bindings cover Index/Knuckles;
  qualify/add other target controllers explicitly.
- Provide consistent head validity/pose reporting. This plugin creates controller
  trackers but Prim currently checks XRServer's head tracker for validity.
- Review action update ordering (currently pose reads precede UpdateActionState),
  texture-copy/submission synchronization and orderly GPU teardown.
- Add genuine hidden-overlay behavior: stop eye submissions/render work while
  retaining required tracking/input, rather than merely setting alpha to zero.
- Test the zero-prediction head-pose sample and application-driven render pacing;
  do not promise scene-quality reprojection from projection-overlay API support.
- Compile against Prim's Godot API and pinned OpenVR SDK on Linux and Windows.
  The repository documents Windows use; Linux build configuration exists but is
  not current Linux runtime qualification.

Prove an isolated rendering path before altering the working SteamVR handoff:
- Read controller grip/sticks and supply gesture haptics alongside another game;
  current helper sends poses/status only. Check current controller profiles without
  acquiring foreground input or unintentionally replacing action bindings.
- Submit per-eye projection overlays from a tiny Godot scene. Check correct eye
  separation, projection, tracking origin, texture lifetime and pose/render timing.
- Prove overlay/OpenXR scene ownership and orderly teardown coexistence. Test
  both launch orders and return to the current tracking-only backend after failure.

Prefer GPU texture submission near the renderer over round-tripping image pixels
through the helper's JSON pipe. Keep pose/status ownership behind the existing
backend boundary. Start with the plugin in the renderer and the existing read-only helper for
status. Qualify sequential OpenXR-scene/OpenVR-overlay initialization and cleanup
in that process before committing to it. If identity/lifecycle conflicts remain,
then evaluate helper-owned handles and explicit GPU sharing as a separate gate. Avoid introducing a full scene-state IPC
protocol solely to make this first rendering experiment possible.

Reuse reveal state, room transforms, fade, locomotion and watch controller logic.
Measure foreground-game frame timing, Prim CPU/GPU/VRAM and subjective latency
while hidden, revealing and visible. Test rapid head rotation, translation,
recenter, wrist readability, repeated show/hide and switching games. No fixed
performance claim follows from API support. After the user's headset comparison,
keep stereo if acceptable; otherwise compare the mono 2 m quad on the same scene
and hardware. Curvature is optional polish. Background tracking remains usable
regardless of the presentation result.

Primary API reference: Valve IVROverlay, including SetOverlayTransformProjection,
SetOverlayTexture, tracked-device transforms and curvature:
https://github.com/ValveSoftware/openvr/blob/master/headers/openvr.h

### 5. Separate resident voice/tracking proof

This is a separate work block, not a prerequisite for controls or OpenVR peek.
Measure the current hidden baseline first; no “Discord/Mumble-light” performance
claim before measuring. Target no 3D scene/GPU rendering in the resident component.

The resident component should own session identity/connections, voice capture and
encoding, receive jitter/decode, HRTF mixing/output, microphone/deafen state, and
tracking/provider selection. The Godot process owns room/avatars/video rendering
and menus. Current networking already has a worker boundary, but PrimSession
creates Godot AudioStreamNetwork objects and voice HRTF lives in SteamAudioPlayer
nodes; moving sockets alone will not preserve audible voice through renderer loss.

Extract/test the native voice core and a small point-source Steam Audio mixer
without Godot nodes. Keep mute/deafen authoritative in one component and use a
bounded local IPC API for commands, latest poses and state snapshots. Do not put
network secrets in renderer command lines or duplicate ownership of XR providers.
Renderer attaches/detaches without reconnecting peers; loss of a pose source is
explicitly invalid, with timestamps/recenter epochs and a defined fallback.
Retain viseme inputs/envelopes for a later attached renderer without making voice
depend on avatar rendering.

Proof: two clients connected, physical head turning changes HRTF; close/kill only
the renderer and retain voice, mute/deafen and remote head/hands; restart renderer
and recover the same room without duplicate voices/participants. Local media can
stop; the service must not emit a room-wide stop command as a side effect.

Only after that, test a small trusted mech-vr/Mumble-Link-style adapter. Keep
physical playspace poses for social body language separate from game-space voice
emitter/listener coordinates. Context IDs/units/reference transforms and which
participants share game-space audio need their own explicit contract. Starting a
game does not automatically grant it authority over room identity or everyone's
voice routing. Do not include host migration in this extraction.

## Next checks

Retry the friend's Arch/Envision/WayVR setup with the compatibility option.
Check the final headset-facing wrist layout, independent deafen with friends and
media, then opt-in Windows SteamVR stereo with Index and streamed Quest setups.
Keep the foreground game running for a useful latency/performance comparison.
If stereo is objectionable, compare the planned mono quad fallback before adding
more rendering machinery. Step 5 requires its own later plan/work block.
