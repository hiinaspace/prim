# Arch / Envision / WayVR companion retry

These changes are in the development checkout after 0.3.8; the published 0.3.8
build does not contain them. Use the normal development build instructions in
[BUILDING.md](BUILDING.md), or a new build supplied by the maintainer.

## Short headset test

1. Start the same Envision runtime and WayVR that showed only the background.
   Enable VR in Prim. Note the status text before opening another game.
2. If it says **Activity unknown**, try the lift gesture or **Reveal Prim room**
   in the desktop menu. Unknown activity now permits deliberate reveal while
   keeping ordinary Prim controller buttons/lasers disabled.
3. If WayVR's background still covers Prim, select **Comfort → Draw Prim above
   other overlays**. This restarts only Prim's XR session; voice/room stay alive.
   Reveal again. This mode intentionally covers WayVR panels as well as its
   background. Leave it off when cooperative WayVR background suppression works.
4. Start a real VR game. Check hidden tracking, lift/release to latch Prim open,
   pull down to hide, and quick peek without releasing. Click the desktop scene
   for mouse yaw; Tab/Esc or leaving the window releases the pointer.
5. During peek, first release both sticks, then test movement/turning. The game
   receives the same stick input. Prim's normal VR menu/laser/mic buttons remain
   disabled. Look at a wrist target to mute or deafen. Deafen silences incoming
   Prim voices/media only: a green **MIC ON** can coexist with **DEAF**.
6. Close the game, disable/re-enable VR, and confirm room/voice continuity. When
   activity is known idle, Prim should return automatically; if status remains
   unknown, explicit reveal is still the fallback.

If visibility still fails, compare once with WayVR closed and report the status,
whether the explicit Reveal button works, and whether the compatibility option
changes anything. This separates input/status failures from layer ordering.

## Notes for a contributing agent

The supplied support ZIP showed a 0.3.8 session with Monado
`v25.1.0-505-g3170a8855`, but libmonado status IPC from
`v25.1.0-294-ga3574442e`. Tracking worked; the optional status connection failed.
The old policy also discarded deliberate reveal whenever status was unknown.
Older 0.3.6 loader errors in that ZIP are a separate issue.

Relevant code:
- `native/src/xr_monitor.rs`: canonicalize the selected runtime manifest, resolve
  relative `library_path`, try its library directory before system libmonado,
  and require successful IPC/polling before choosing a candidate. An explicit
  `PRIM_LIBMONADO` remains first. Failures back off for 15 seconds.
- `project/xr/session_lifecycle.gd`: explicit reveal with unknown activity;
  placement 1 (cooperative) versus 10 (above other overlays), selected when the
  XR session is created. Unknown status never automatically grants scene input.
- `tools/test-monado-status.py`: a symlinked Envision-style manifest, rejected
  override library, and a working runtime-local libmonado fixture.
- `project/tests/xr_policy.gd`, `tools/test-xr-companion.py`: unknown-status policy,
  real private Monado session restarts/placement and companion controls.

Do not set `IPC_IGNORE_VERSION` globally to bypass private service ABI checks.
If runtime-local lookup still fails, inspect where this Envision installation
actually puts the matching libmonado and its transitive dependencies. The log's
`PRIM_XR_MONITOR connected status_library=...` identifies the successful client;
otherwise the bounded error lists attempted candidates. Capture a fresh support
ZIP and exact runtime/WayVR versions. Do not include `private_lobby.json` or copy
raw personal support logs into a public issue.

Locally qualified: rejected-client fallback, deliberate unknown-state reveal
policy, real Monado placement 1/10 and XR-only restart, tracking/remote poses and
companion input gates. The friend's exact unpatched WayVR/Envision composition
still needs this retry; local cooperative WayVR alone cannot prove that result.
