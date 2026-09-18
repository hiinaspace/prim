# Desktop and VR in one Prim session

September 17 companion update: the staged Monado backend now uses a persistent
overlay and a separate headset viewport. See [companion behavior and tests](XR_COMPANION_TEST.md).
The scene-session implementation and earlier validation below remain the baseline
for non-overlay runtimes and the original re-entry fixes.

Prim starts its room, playback and microphone independently of OpenXR. The normal
launch tries VR once and falls back to desktop. `./run.sh --desktop` skips that
initial attempt; **Enter VR** in Room can start it later. **Leave VR** returns
to desktop and releases the OpenXR instance/session. Runtime loss also returns to
desktop. Re-entry is explicit, so starting a runtime for another game does not
make Prim grab the headset again.

On exit, Prim reapplies the Window's existing content-scale settings immediately
after disabling XR. Godot otherwise retains the headset render dimensions and
cleared 2D size override until the next window resize, making the desktop menu
small and its mouse coordinates incorrect. The user confirmed that resizing the
affected live window restored both appearance and interaction. The menu regression
now uses a size-only XR interface to exercise actual viewport resizing and the
exit path at three desktop scales without touching a headset runtime; it failed
before the fix and passes afterward. The user subsequently confirmed the menu
works after the fix; it ships in Linux/Windows 0.3.6. See
`.local/menu-desktop-return/viewport-*.log`.

The desktop window/process, room membership, shared media and mute setting stay
alive. The audio listener follows the active camera. Entry aligns the first
tracked head pose with the desktop seat; exit retains the head position and view
direction. Tracking/controllers stop when leaving VR. Continuing physical head
and hand tracking while another game runs is the separate invisible-tracking
proof, not a feature of this scene-session change.

## Afternoon test card

1. Start `./run.sh --desktop` with the runtime off. Connect/play media if desired.
   Enter VR should fail recoverably; the desktop room remains usable.
2. Start the normal headset runtime, press Enter VR, and check head/hands, room
   position, menu input, microphone and spatial audio. OpenXR becoming focused is
   not evidence that a real headset is comfortable or correctly rendered.
3. Leave VR. Confirm the window works, the movie continues and friends still hear
   you. Try another scene app while Prim remains on desktop, then close that app
   and re-enter Prim VR. Repeat twice, including with the menu hidden.
4. Stop/restart the normal runtime once. Prim should remain in desktop and allow
   explicit re-entry. Check audio routing too: headset-device disappearance is a
   separate OS/audio-backend concern from preserving the Prim audio nodes.
5. Run the subtitle, non-host sharing and stereo comparison card in
   `docs/BETA_PLAYBACK_FOLLOWUP_PLAN.md`. With a friend, toggle modes during a movie
   and conversation; record any audible gap, pose jump or reconnect.

## Engine boundary

`build-support/godot/default.nix` adds the restartable OpenXR and stereo re-entry patches
after the three media patches. `tools/build-godot.sh` builds that derivation. `build.sh` always
resolves it, preventing reuse of a staged engine that lacks the lifecycle API.
The Windows engine needs both additional patches; existing Windows packages do not
acquire this change from a GDScript-only export.

The opt-in project setting `xr/openxr/runtime_switching` postpones XR instance and
session initialization until the application asks. Vulkan uses
`XR_KHR_vulkan_enable` for an application-created desktop device. The engine
records enabled Vulkan extensions and the actual GPU/queue, then verifies them
against the runtime before attaching. A different required GPU or an unavailable
required extension produces a desktop fallback. This does not recreate the
renderer to move between GPUs, or promise arbitrary runtime switching in one
process. Prim's Vulkan path is the target; other renderer backends are unqualified.

Leaving requests `xrRequestExitSession`, handles STOPPING/EXITING, detaches the XR
viewport, drains buffered rendering frames and synchronizes before destroying XR
resources. Runtime loss is reported to the app instead of ending the room. The
application controller lives in `project/xr/session_lifecycle.gd`; main owns the
camera, controller presentation and room-space alignment.

## Automated checks

`nix develop --command python3 tools/test-xr-lifecycle.py` runs two local Prim
processes, synthetic voice on private PulseAudio sinks, a shared file, and an
isolated Monado QWERTY/null compositor. It exercises unavailable-runtime entry,
late entry, three leave/re-entry cycles, runtime termination and recovery. It
checks persistent room/media/capture identities, remote voice/pose/playback
progress and active-camera/listener ownership. A test-only compositor observes
actual render-thread buffer and scene view counts (two in VR, one on desktop)
and physical eye separation on every entry. The runner rejects rendering/script
errors even if scene assertions pass. Logs/results are written under
`.local/xr-lifecycle-tests/<timestamp>`. No physical headset or normal VR service
is used. `GODOT` may override the staged engine.

The regular menu, playback-controls and two-process integration checks remain
relevant to the existing media controls. Native Windows/SteamVR, WiVRn, physical
controller input, audible headset continuity and a VTOL handoff require human
validation; simulated OpenXR does not establish those results.

## Stereo re-entry regression

The first human test confirmed initial entry and exit, but subsequent entries
looked cross-eyed/nonstereo. A render-thread probe reproduced the discrepancy:
first entry had two buffer views and two scene views; later entries had **one
buffer view and two scene views**, despite correct 63 mm eye separation in the
synthetic runtime. The earlier lifecycle checks did not inspect render buffers.

Godot's renderer resets its viewport to one view on XR exit, while the scene
Viewport cached two. Re-entering at identical dimensions then skipped the stereo
resize. Patch `0005-restore-stereo-on-xr-reentry.patch` resets that scene cache to
one alongside the renderer. It preserves the existing render-target teardown
order. The regression fails on the previous engine and passes with this patch;
the user subsequently confirmed repeated physical-headset switching works
without apparent state carryover.

Before-fix evidence: `.local/xr-lifecycle-tests/20260917-151701/`.

## Verified September 17 candidate

The final Nix engine is staged at `.local/godot`; `./run.sh --desktop` is ready
for the human card above. Engine output:
`/nix/store/0sidjcd0np64scmvs0ff5l4kkprzbwcd-godot-4.7.2-stable`.
Source remains `main` at `015ce98` plus uncommitted changes; no release was made.
`.local/xr-lifecycle/candidate.json` records executable/source hashes.

- 137 real Vulkan/OpenXR lifecycle checks passed, plus the marker confirming
  graceful close from an active VR session. Six XR entries included three full
  cycles, forced runtime termination/restart and focused-session shutdown.
  All six entries used two render-buffer and scene views with 63 mm eye separation;
  all four explicit exits returned both counts to one. No rendering errors.
- Both synthetic controllers and the head tracked after every entry. A second
  process received advancing shared-file playback, voice and poses across changes.
- Final logs show no leaked XR resources. Expected unavailable-runtime and
  broken-IPC diagnostics occur during deliberate failure injection.
- 36 menu checks passed again on this engine. Earlier 29 subtitle/stereo/playback
  control checks and 23 launcher self-checks passed. The final editor import had
  no script errors.
- Lifecycle evidence: `.local/xr-lifecycle-tests/20260917-153204/`;
  re-entry build/test/menu/import logs: `.local/xr-lifecycle/reentry/`;
  earlier controls/launcher logs: `.local/xr-lifecycle/{controls-final,launcher}.log`.

This is a Linux checkout candidate. Existing `.local/beta-linux` and
`.local/beta-windows` packages were not refreshed; the Windows engine has not
been rebuilt with this patch. Physical headset comfort/input, audio-device
changes, VTOL handoff, WiVRn and native Windows/SteamVR remain human/platform gates.
