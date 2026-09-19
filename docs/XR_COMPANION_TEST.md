# Monado companion VR: implementation and test card

Historical implementation/test record from September 17, 2026. Several behaviors
below were superseded: see [companion follow-up](COMPANION_NEXT_PLAN.md) for tracked
desktop yaw, peek locomotion, explicit reveal on unknown status and wrist controls,
and [OpenVR qualification](../build-support/openvr-overlay/README.md) for SteamVR.
Start with `./run.sh --desktop`, then **Enable VR**. The current Linux engine and
native extension are staged by `./build.sh`; `./run.sh` launches without rebuilding.

## Behavior

- On Monado, Prim requests a persistent OpenXR overlay at placement 1. With no
  other main session, it shows the room with normal VR controls. When a game
  starts, it hides its projection and gates its controller UI/actions. On game
  exit it automatically returns to the interactive room, without recreating XR.
- Physical head/hands continue driving the avatar and spatial listener while
  hidden. Game/vehicle movement does not enter Prim's coordinate transform.
- The desktop window has an independent head-following view. Focused WASD moves
  the room anchor; Tab/Esc opens its 2D mouse menu. The desktop menu never appears
  in the game/reveal headset view. Mouse look does not change the tracked avatar.
  In the primary Prim room, a desktop menu does not block VR controls. Opening
  the VR menu transfers the shared panel to the headset and closes the 2D panel.
- Hold either controller in front of the face for approximately 500 ms with grip
  released. A brief haptic pulse signals readiness. Grip and lift about 11 cm. While held, room opacity follows the lift.
  Movement produces short haptic pulses scaled by hand-to-head vertical speed.
  Lower and release for a quick peek, or release fully raised to latch the room.
  From a latch, grip near/slightly above the head, lower and release to return.
  A partial opening release cancels; a partial closing release restores the latch.
- Prim leaves controller input available to the game. Its normal controller
  actions stay blocked even during a latched reveal. On returning to full Prim,
  controls must first return to neutral. The desktop **Hide room view** button is
  an escape path for a revealed overlay.
- Disable VR preserves the room, player and voice objects and returns to desktop
  poses. Tracking loss cancels a peek; invalid head tracking hides the projection.
  Runtime failure retains the existing explicit retry behavior.

No global FPS limit was added. Hidden XR skips scene drawing and submits zero
layers; the desktop preview currently keeps rendering. Lower-rate rendering of
an unfocused preview is deferred so tracking/gestures and background audio/media
remain unaffected by a blanket application cap.

## Components and scope

`project/xr/session_lifecycle.gd` owns lifetime, runtime observation and presentation
policy. `native/src/xr_monitor.rs` uses the optional versioned libmonado C API on a
worker, with copied client names and stale-state handling. It does not change
focus, primary role or input blocking. The worker is joined before native-library
unload. Core OpenXR visibility events immediately gate controls; authoritative
client state distinguishes a hidden game from a game that exited.

`project/xr/reveal_gesture.gd` is a pose/button state machine. The reveal compositor
effect changes both projection eyes' alpha; it does not move the camera. The
engine patch `0006-companion-overlay.patch` supplies optional overlay creation,
visibility events, source/unpremultiplied alpha, queued render suppression and
no cross-session depth submission. Desktop and XR use separate viewports sharing
the same room world. The existing graceful exit/GPU-drain path is retained.

The [WayVR companion patch](../build-support/wayvr/README.md) keeps WayVR at
placement 5, above Prim, and suppresses only its skybox while Prim is active.
It was applied to `/home/s/code/wayvr` and the debug service was restarted;
Monado itself was not restarted. No persistent WayVR setting was changed.

This initial record qualified Monado. `PRIM_XR_SCENE=1` selects the ordinary scene
role for development comparisons. SteamVR background tracking and experimental
peek were added later; unknown Monado status now permits deliberate reveal while
ordinary scene interaction remains gated. Follow the documents above for current
behavior and outstanding platform checks. True headless operation remains separate.

## Recorded evidence

Artifacts are under `.local/xr-companion/`:

- Native runtime classification: overlay exclusion, unfocused game ownership,
  primary-main selection; 3 tests pass (`native-verified.log`).
- Gesture traces: 13 checks pass, including quick peek, latch, either-hand close,
  partial releases, held-input entry and tracking loss (`gesture-tests.log`).
- Runtime policy: 12 checks pass for stale IPC, visibility versus exit, immediate
  action gating, automatic return, head loss and desktop hide (`policy-final.log`).
- Menu regression: 75 checks pass with isolated settings, including real pointer
  button/dropdown callbacks and legacy XR viewport resize recovery. Final rendered
  result is `menu-rendered-final.log`. Linux menu tests now reject an unset
  `XDG_DATA_HOME`, preventing accidental use of the normal profile.
- `tools/test-xr-companion.py` runs a private Monado/QWERTY runtime, a separate
  OpenXR main-session app, and a second networked Prim observer. It verifies two
  attach/detach orders, stereo buffers/IPD, hidden draw suppression with continuing
  desktop frames and tracked poses, desktop menu callbacks, anchor movement,
  both-eye GPU alpha readback, automatic return and persistent room objects.
  See `final-coexistence.log` for the final artifact directory.
- `physical-smoke.log`: two live Monado cycles detected the HMD and both real
  controllers (`tracked=7`), retained overlay role, returned to desktop cleanly,
  and caused WayVR background-owner true/false transitions each time. No new
  NVIDIA Xid appeared during that check. This was telemetry, not a headset visual
  or physical gesture acceptance test.

The new coexistence test initially caught a second-entry controller-policy reset;
it is fixed and covered. An initial GPU test attempted unsupported texture
readback; the final test reads one alpha per eye via a compute buffer instead.
The final test runs contain no rendering/script/resource errors.

One menu run mistakenly used the normal settings profile. The last recorded
profile was restored (voice 285%, 3–15 m; movie 6–18 m; spatial audio; Vita/1.75 m),
with the post-test file preserved in `settings-after-mis-scoped-test.cfg`.
More recent user adjustments cannot be reconstructed from that earlier snapshot;
the user was notified and asked about them. Subsequent tests use isolated profiles.

## September 17 headset feedback and follow-up

The user confirmed mech-game coexistence, reveal/return gestures, effective
crossfade and correct WayVR ordering in a physical headset. Follow-up changes
halve lift travel to 11 cm, add ready/motion haptics, and restore VR controls
with the desktop panel open. Closing travel starts at full opacity even at the
lower latch height.

Godot skips sky drawing for transparent viewports. The XR viewport now draws
the full environment; the existing post-transparent compositor independently
sets reveal alpha. The private runtime test samples geometry-free sky pixels
and verifies nonblack RGB and half alpha in both eyes. It also injects tracked
controller inputs to verify joystick movement and desktop-to-headset menu
transfer. Evidence: `.local/xr-companion/followup-coexistence.log` and
`coexistence-20260917-232106/`. Gesture feedback tests cover one-shot readiness,
velocity-scaled pulses, shared head/hand movement, rest, release and tracking
loss (`followup-gesture.log`). The user subsequently accepted the updated settings in a physical headset.
All 177 follow-up checks passed (22 gesture, 12 policy, 75 menu, 68 coexistence).
Before another release, test SteamVR/OpenXR without overlay support: ordinary
scene fallback, repeated Enable/Disable VR, stereo, and desktop/VR menu input.
This platform gate is intentionally deferred to the next session.

## Headset acceptance card

1. Enable VR with WayVR running. Prim should supply the room; WayVR's dashboard
   should appear above it without its skybox covering Prim. Open the Prim VR menu
   and verify buttons and dropdowns.
2. Launch VTOL (or mech-vr). Prim should hide automatically. Its desktop Tab menu
   and WASD should remain usable; the game should still receive controller input.
3. Test a held quick peek and a latched reveal, then close with either hand. Check
   stereo, opacity and motion comfort. Prim's headset menu/laser must stay absent
   throughout the game; the desktop menu must remain desktop-only.
4. Exit the game. Prim should return automatically; release held controls before
   operating its VR menu. Disable/re-enable VR twice and check both input paths.
5. With an observer, confirm recognizable head/hand movement and room-anchored
   voice during gameplay. Compare game performance with Prim disabled, hidden
   and revealed. Verify background music/live media and game-only audio routing.

Actual VTOL input coexistence, subjective headset composition/gesture feel,
sustained performance, native Windows/SteamVR and WiVRn remain human/platform
gates. Public release, capture automation and headless/SteamVR backends are not
part of this staged change.
