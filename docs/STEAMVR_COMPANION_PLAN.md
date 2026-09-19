# SteamVR companion fallback: tactical plan

Implementation checkpoint, September 18, 2026. Native SteamVR background tracking
and automatic OpenXR scene handoff are implemented and tested locally.
The accepted Monado overlay path remains in place. This is the next backend
qualification gate after Prim a6f28d1 and libmpv-zero 293d895.

## Decisions from the interview

- Target Windows SteamVR first; Linux SteamVR is useful for local probes.
- OpenVR is acceptable for background tracking and potentially overlays, but
  keep it behind a small backend boundary rather than making it the product API.
- Treat SteamVR Home as idle. Dashboard visibility during a game is not idle.
- Add an explicit **Open Prim in headset** action for ambiguous ownership.
  The user approved replacing a real game after confirmation that SteamVR may
  close it. Automatic behavior must never perform that takeover.
- A brief tracking gap during handoff is acceptable. Room membership, voice and
  media must continue; do not reconnect the room to change tracking providers.
- Keep the current focused desktop mirror/menu/WASD behavior. No flip-up gesture
  on the initial SteamVR tracking-only path. OpenVR rendering is a later option.
- Base stations were powered through vr-lighthouses.service; leave them on.

## Evidence and the change to the proposed headless route

The local installed SteamVR is **2.17.10**, build 25330290. Enumerating its real
Linux runtime returned 40 extensions, including XR_MND_headless revision 3 and
excluding XR_EXTX_overlay. Public 2.17/2.17.10 stable/beta notes inspected today
do not announce OpenXR overlay support. This does not establish what an
unavailable/internal Valve alpha supports.

Valve's March 2023 developer reply says SteamVR OpenXR headless applications
still occupy the exclusive scene slot. **Confirmed on this installed runtime:**
a disposable OpenVR scene survived an OpenVR Background client, then received
`VREvent_Quit` when the headless OpenXR probe created its session. SteamVR's
server log registered that probe as `VRApplication_OpenXRScene`. Use OpenVR
Background for the companion provider on this runtime.

The extension removes graphics requirements; its presence does not prove
coexistence. An ordinary OpenXR quad layer is not an independent cross-application
overlay. OpenVR overlays remain a separate option for a later peek implementation.

Local evidence:
- `.local/steamvr-plan/extensions.log`: actual runtime enumeration.
- `.local/steamvr-setup/coexistence-result.log`: Background exit 0, headless
  exit 0, disposable scene exit 5 after `QUIT_REQUESTED`.
- `.local/steamvr-setup/headless-eviction-vrserver.txt`: runtime ownership events.
- User confirmed Beyond tracking, SteamVR dashboard and SteamVR Home work.
- `stop-steamvr` restored the exact Home Manager XRizer symlink; a second
  `start-steamvr` brought up the compositor and Home again. Lighthouses stayed on.
- First real Prim scene test exposed a missing desktop-device Vulkan extension:
  `VK_KHR_timeline_semaphore`. Patch 0007 requests it for runtime switching;
  the media interop patch already enables its supported device feature.
  The incrementally rebuilt engine passed 14 live checks: fresh stereo/IPD on
  two entries, head and both hands (tracked mask 7), game eviction, survival
  for 45 seconds, orderly disable, and stable party/media/microphone objects.
  This does not measure audio/network delivery or native Windows behavior.
- The reproducible Nix engine build completed; the existing private Monado
  companion regression passed all 68 checks with it. Evidence:
  `.local/xr-companion/coexistence-20260918-195253/`. The packaged engine
  also passed two real SteamVR scene enter/exit cycles; see
  `.local/steamvr-setup/nix-scene-smoke.log`.
- The isolated packaging fix `434b3b3` was fast-forwarded into main separately.
  Linux 0.3.7 is already published; Windows remains 0.3.6. Do not reuse 0.3.7
  for the next companion feature release.

The display-lease failure was missing `WAYLAND_DISPLAY` in the launch environment.
The installed Nix `start-steamvr` service uses the graphical-session environment,
Sniper runtime, and a temporary native OpenVR registry selection. `stop-steamvr`
restores the exact previous registry file/symlink. `steamvr-run COMMAND` selects
SteamVR for a test command; the normal OpenXR default remains Monado. Do not run
the two runtime servers together. No driver change or reboot was needed.

Sources:
- [Valve headless/scene explanation](https://steamcommunity.com/app/250820/discussions/8/3783625016029084617/)
- [Headless specification](https://raw.githubusercontent.com/KhronosGroup/OpenXR-Docs/main/specification/sources/chapters/extensions/mnd/mnd_headless.adoc)
- [Valve OpenVR API](https://github.com/ValveSoftware/openvr/blob/master/headers/openvr.h)
- [SteamVR release announcements](https://store.steampowered.com/oldnews/?appgroupname=SteamVR&appids=250820&feed=steam_community_announcements&l=french)
- [Khronos runtime inventory](https://github.khronos.org/OpenXR-Inventory/runtime_extension_support.html)

## Target behavior

| Situation with VR enabled | Tracking | Headset presentation |
| --- | --- | --- |
| Monado with qualified overlay support | Existing OpenXR tracking | Existing automatic room/reveal behavior |
| SteamVR actual game running or starting | Background provider | Game only; no Prim gesture or VR actions |
| SteamVR idle or known SteamVR Home | OpenXR scene once ownership is stable | Interactive Prim room |
| SteamVR ownership unknown/stale | Background if available, otherwise invalid tracking | Do not automatically claim the scene |
| Handoff underway | Brief explicitly invalid/frozen presentation interval | Preserve room connection and audio/media |
| VR disabled | Desktop pose | No Prim XR or background tracking attachment |
| Runtime disappears | Desktop fallback/status with explicit retry | Preserve Prim process and room |

Use an allowlist of known Home application identity, not a fuzzy process-name
match. Exclude Prim's own process(es), inspect starting application/transition
state, and debounce idle before claiming the scene. Loss of dashboard focus or
a temporary lack of submitted frames must not mean that the game exited.

Keep Enable VR as durable user intent, separate from live XR session state.
A normal scene being evicted by a game should transition to background tracking,
not turn off VR intent or close Prim. Runtime loss is a different transition.

## Build sequence and gates

1. **Resolve the capability and lifecycle questions in a disposable harness.**
   Enumerate actual extensions before scene creation. Use OpenVR Background for
   read-only scene PID, app identity and transition state. Run a tiny headless
   OpenXR probe beside a disposable OpenVR scene and a disposable OpenXR scene;
   record ownership and quit events in both launch orders. Only select headless
   on a runtime/version actually qualified for coexistence.
   Also prove release of an OpenXR scene when another game starts, keeping the
   owning process alive after orderly session/instance destruction. A quit
   acknowledgement alone is not proof: Valve documents a forced-kill timeout.
   This result determines whether an in-process scene is safe.
   Linux display-lease repair gets a bounded attempt; otherwise make this harness
   an early Windows diagnostic for a friend before building the full feature.

2. **Add a SteamVR status and tracking adapter.**
   Extend the existing native monitor's common snapshot: runtime, connection,
   scene identity, starting/idle/other/unknown, observation age and capabilities.
   Do not call PerformApplicationPrelaunchCheck during monitoring: despite its
   name, Valve documents that it asks the existing scene to quit.
   Prefer a small separate Background helper with a bounded local IPC stream,
   avoiding OpenVR/OpenXR client-registration conflicts inside the Godot process.
   Compare in-process initialization in the harness before choosing to simplify.
   Sample head/controllers at a useful tracking rate, not the status poll's 2 Hz.
   Return standing-space transforms, validity, timestamps, role mapping and
   origin/reset information; avoid action capture and rendering initially.
   Verify controller grip orientation/offset against the existing OpenXR avatar.
   Detect runtime/API shutdown and stale poses; do not display old poses as live.

3. **Separate tracking from presentation in Prim.**
   Current main.gd uses xr to mean tracking, XR camera/listener and rendering.
   Introduce an explicit tracking provider and presentation role, reusing avatar
   packets, room anchor and audio listener behavior.
   Background mode keeps physical head/hands and room-anchored spatial voice,
   desktop menu/mirror and focused WASD. Disable all Prim controller actions and
   reveal haptics/gestures while the other game owns the headset.
   Preserve position/yaw/physical floor alignment when switching OpenXR and
   OpenVR providers; reset spring/pose interpolation without snapping rooms.

4. **Integrate scene handoff, then the UI.**
   Start background tracking when a game exists. When genuinely idle/Home,
   release conflicting registrations, recheck ownership immediately, and start
   Prim's ordinary OpenXR scene. On another game's launch or normal eviction,
   promptly release XR and resume background tracking without dropping party
   intent. A stale observation is not permission to start a scene.
   Add statuses such as "Tracking alongside VTOL", "Prim in headset",
   "Waiting for SteamVR" and "Checking active app".
   Add "Open Prim in headset" for explicit takeover; warn when a known game may
   close and require that deliberate choice. Never automatic takeover of a game.
   Disable VR cancels pending starts and helper reconnection.
   If SteamVR still kills the owning Godot process despite orderly XR release,
   stop here: scope a separate scene-render process, or defer automatic scene
   handoff. Do not ship a path that can kill the party process.

5. **Build and release for friends to test.**
   Include helper/runtime dependencies and diagnostic logs. Do not require users
   to change their global OpenXR runtime merely to use an overlay or read poses.
   Detect the actual runtime: Virtual Desktop users may be using VDXR rather
   than SteamVR OpenXR. That runtime is outside this first companion target.
   Exercise ordinary scene-only fallback explicitly, then Windows exports and
   native Windows tests with friends. Prim is already experimental/alpha; use
   the normal release procedure without a separate candidate label or rollback
   process. Include a short testing checklist and known limitations.

## Validation order

- Pure policy fixtures: Home, dashboard, starting app, stale data, rapid game
  start/exit, disable mid-transition, missing helper/headset/controllers, runtime
  loss, and explicit takeover. Ensure no oscillation or accidental game takeover.
- Existing private Monado harness: force the fallback policy, headless session
  or synthetic provider as appropriate; repeat real session attach/detach and
  check stereo, menus, room identity, audio/media and pose interpolation.
  This validates Prim's state machine, not SteamVR exclusivity or forced quits.
- Real SteamVR: both game-first and Prim-first, OpenVR and OpenXR games,
  background pose availability, eviction handling, stable return and repeated
  transitions. Test Home enabled and dashboard open. Check correct reference
  space and listener direction across recenter/turn/movement.
- Windows package: executable/DLL loading can be checked under Wine, but runtime
  coexistence needs native Windows. Test Index and one Quest streaming setup.
- Two Prim clients: ensure an observer actually receives moving head/hands and
  voice through all transitions. A solo test alone does not qualify that path.
- Rerun the accepted Monado overlay/gesture/menu tests before the friends build.

## Short solo Windows friends card (about 10–15 minutes)

1. Select SteamVR as the runtime for this test; record SteamVR version/headset.
   Start Prim desktop. Enable VR while idle/Home; verify room, stereo and menus.
2. With Prim running, launch the chosen VR game. Confirm the game and Prim both
   stay alive. Prim desktop should show tracking status and moving head/hands.
   Move/turn physically and check that the avatar reflects you, not game motion.
3. Open/close SteamVR dashboard. It must not make Prim replace the game.
   Use Prim's desktop menu via desktop view/another overlay; no Prim gesture.
4. Exit the game. Prim should appear automatically. Repeat the cycle twice.
5. Quit Prim, start the game first, then Prim/Enable VR. The game must remain.
   Disable/re-enable VR during the game. Test desktop menus after leaving VR.
6. Play media/music through transitions; test microphone using the available
   local check. Send the diagnostic bundle and the failed step, if any.

Separately do one short two-person session for actual remote voice and tracked
avatar delivery. This checklist defines the first tracking-only milestone.
OpenVR peek rendering and other runtimes can follow separately.

## Effort / next decision

First work block: capability/eviction harness and basic background adapter.
Next: Prim tracking/presentation separation and handoff. Then a Windows build
and friends feedback. Budget roughly 2–3 focused implementation days if normal
scene release preserves the process; a separate renderer or sustained local
SteamVR graphics repair is a new scope decision rather than a hidden addition.

Resolved: the user approved deliberate known-game takeover after confirmation.
The automatic policy still leaves games alone.

## Local SteamVR / Beyond setup follow-up

At the user's suggestion, checked the current Linux VR Adventures Beyond,
SteamVR quick-start and NixOS pages. The Beyond page lists NVIDIA open-module
and DSC setup plus device access requirements. The host is running NVIDIA
615.71.09 open modules, and /home/s/nixos-config/configuration.nix already
applies bsb-dsc-fix.patch. The probe receives valid tracking.

The initial Compositor_CannotDRMLeaseDisplay failure was resolved by passing
WAYLAND_DISPLAY through the graphical-session launcher. No additional Beyond
driver, firmware change, or kernel update was needed. User confirmed Home,
dashboard and headset tracking. The Nix setup and recovery procedure is recorded
in `/home/s/nixos-config/docs/steamvr-native-20260918.md`.

The launcher selects SteamVR only for its running session. Stop it before normal
Monado use; base stations stay on. Global OpenXR remains Monado. Home Manager
rebuilds can replace the temporary registry; cleanup recognizes identical restored
configuration and preserves genuinely changed selections for inspection.

### Repeatable Linux scene gate

Run `./tools/build-steamvr-probe.sh`, start native SteamVR, then run
`./tools/test-steamvr-scene.py --allow-scene-takeover` from the graphical shell.
Wear/move the headset so standby does not suppress rendering. This deliberately
replaces the current VR scene with Prim, then a disposable game, then Prim again.
It writes isolated profiles/logs under `.local/steamvr-tests/`. It requires the
patched engine and existing staged Prim extensions. The raw probe's `background`
mode is read-only; `scene` and `headless` modes can evict the current scene.

OpenVR background tracking and explicit provider handoff now pass the live
companion harness. The initial in-process OpenXR scene lifetime gate passed locally, so a
separate scene-rendering process is not required by current evidence. Keep the
Windows eviction/survival check in the friends testing checklist. OpenVR peek
rendering follows the first tracking-only release.

- [LVRA Beyond setup](https://vronlinux.org/docs/hardware/bigscreen-beyond/)
- [LVRA graphics troubleshooting](https://vronlinux.org/docs/hardware/graphics-cards/)
- [LVRA SteamVR setup](https://vronlinux.org/docs/steamvr/quick-start/)
- [Niri display-leasing investigation](https://github.com/niri-wm/niri/pull/3738)


## Implemented companion and validation

`native/openvr-helper` is a separate read-only Background client. Prim requests
one bounded pose/ownership snapshot at a time; stale responses cannot trigger
a scene claim. It uses standing-space head/controller poses, selecting
`openxr_grip`, then `grip`, then the legacy `handgrip` model component. Index's
legacy handgrip is about 6 cm away from its OpenXR grip origin; the preferred
component removed that mismatch in the worn-headset comparison.

Enable VR attaches this provider on SteamVR. Stable idle/Home automatically
opens an ordinary Prim OpenXR scene; another game's takeover returns Prim to
background tracking. Desktop WASD translates the playspace; the desktop menu
stays usable and background controller actions do not drive Prim. Background
mode disables desktop VSync and caps drawing at 60 fps (or an existing lower
cap) so occlusion does not stall pose delivery. Disable VR releases the helper.

Local evidence, September 18:
- `tools/test-steamvr-companion.py --allow-scene-takeover`: 35 checks passed,
  including both launch orders, remote avatar delivery, cancellation and
  confirmed takeover, repeated enable/disable, and object/session survival.
  Final artifacts: `.local/steamvr-companion/20260918-202511/`.
- Worn Index-controller comparison: approximately 1.6–2.6 mm position difference
  and 0.6–1.3 degrees rotation difference between independently sampled providers.
  This is a moving sample, not a precision calibration measurement.
- User confirmed room, head/hands, menu and locomotion feel correct in SteamVR.
- Private Monado companion regression passed again:
  `.local/xr-companion/coexistence-20260918-202632/`.
- Linux and Windows helpers build; Windows helper loads under isolated Wine and
  correctly reports no installed runtime there. Windows Rust cross-build passes.
  Updated Windows Godot engine (patches 0001–0007) and package export also pass.
  The packaged Windows app completed a 180-frame Wine desktop Vulkan smoke
  with native extension, Steam Audio and libmpv initialized and exit code zero.
  Wine emitted platform/driver warnings but no Godot script errors.
  Native Windows headset/eviction behavior remains untested.
- Linux package export passes; its bundled loader loads the packaged helper and
  OpenVR library, returning the expected unavailable-runtime error with an
  intentionally nonexistent runtime path. The helper also loads with `/nix`
  hidden (then correctly reports the inaccessible runtime registration).
  This checks dependency packaging,
  not a second live-runtime qualification.
- Menu regression (75 checks), reveal gesture (22 checks), and SteamVR policy
  (19 checks) pass. Stopping SteamVR returned the manual Prim process to desktop
  with an explicit retry status; its process survived and the launcher restored
  the previous OpenVR registry symlink, leaving base stations powered on.

### Short friends test

1. With SteamVR idle/Home, enable VR in Prim. Check stereo, hands, menu and walking.
2. Launch a VR game. Prim should stay connected and switch to background tracking;
   a friend should still see head and hand movement. Check voice/media continuity.
3. Use Prim's desktop menu/WASD while the game owns the headset. Its controllers
   should not activate Prim controls. SteamVR has no Prim flip-up gesture yet.
4. Close the game: Prim should return in the headset. Repeat once.
5. While a disposable game runs, cancel Open Prim in headset, then confirm it.
   Only confirmation should replace the game (SteamVR may close it).
6. Disable/re-enable VR, then quit SteamVR. Prim should survive in desktop mode.

Record Windows/SteamVR version, headset/controllers, the failed step and Prim log
if anything breaks. Published as Linux and Windows 0.3.8; see launcher/hosting/DEPLOYMENT.md.

Local exported builds are `.local/steamvr-implementation/package-linux/` and
`.local/steamvr-implementation/package-windows/`; build/test logs are alongside
them. These tested inputs were packaged and published as launcher update 0.3.8.
