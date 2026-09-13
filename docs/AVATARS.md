# Bundled VRM avatars

The first pass uses Alicia Solid from Mainspring and Vita (`sample_f.vrm`) from
FPSloppa. Select them in the world menu's **Avatar** tab. They are imported at
build time; runtime selection only instantiates catalog scenes. There is no
file picker, download, or peer asset transfer.

## Fit and controls

The quality target is the experimental Alicia setup in Mainspring. Standing
headset-and-hands use is the baseline. RenIK infers hips, legs and feet with
raycasts against the theater floor. No additional trackers are needed.

**Measure / recalibrate standing height** gives a 3.5-second countdown, then
uses the median of valid headset heights sampled in the final half-second.
Stand upright and look forward with a correctly configured tracking floor.
The saved value is eye height, not the top of the head. The slider provides a
manual adjustment between 0.5 and 2.5 meters. It scales the model uniformly without
changing world scale. Desktop retains its 1.60-meter view and corresponding fit;
the saved VR measurement remains separate from that desktop behavior.

Alicia's head-to-view offset is the Mainspring-style approximation
`(0, 0.023, -0.015)` meters before model scaling. Controller grip targets use a
6 cm local-Y wrist shift and the FPSloppa humanoid axis convention. These are
in `project/avatars/driver.gd` and `hand_pose.gd`, respectively. They are deliberate
first-pass constants, not a general controller or avatar calibration system.

Tracked OpenXR/Godot hand joints preserve thumb articulation and finger spreading.
When joint tracking is unavailable, trigger, grip and thumb button/touch inputs
supply a simple curl pose. Untracked hands relax beside the body. UI pointing
continues using the existing aim pose; avatar wrists use separate grip nodes.

The local camera sees the first-person mesh, the preview sees the complete local
mesh, and peers see complete remote meshes. Both representations share one
skeleton. VRM springbones run independently per avatar, including the local body.
The preview is a camera view of the same live pose, not a second solver.

For a single-player microphone/viseme test, open the **Avatar** tab, enable
**Face close-up**, click **Unmute microphone**, and speak while watching the
mirrored preview. No room connection or audio loopback is needed. Choose the
input device and gain in the main tab if necessary. The microphone stays active
when changing tabs and can be muted from either tab or the controller button.
The status reads **MIC LOCAL PREVIEW** while offline. Connecting or disconnecting
resets capture to muted; in a room, unmuting also sends voice to peers.

Overlapping remote bodies hide within 35 cm of the viewer and return beyond 40 cm;
their voice anchor and stream remain alive. This handles coincident spawn points.

Avatar/height changes, snap turns, recentering, large position jumps and long main
thread stalls reset placement/springs. Recenter does not remeasure height. Voice,
names and speaking state remain attached to the tracked head across model changes.

## Implementation boundaries

- `avatars/catalog.gd`: fixed ID/revision registry, imported scenes, metadata bounds.
- `avatars/hand_pose.gd`: tracking/input sampling, canonical wrist-relative joints.
- `avatars/driver.gd`: view/wrist targets, model fit, RenIK placement and modifiers.
- `avatars/fingers.gd`: skeleton modifier translating canonical joints through the
  actual imported parent hierarchy, including Alicia's shared finger helper.
- `net/avatar.gd`: peer presentation, configuration epochs, smoothing and dropouts.
- `net/pose.gd`: validated self-contained v2 snapshots; see [protocol](PROTOCOL.md).

The driver accepts tracking data rather than reading XR devices or network state.
A future Basis/humanoid-pose driver can consume the same input. A general solver
plugin framework is unnecessary at this stage. Full body poses and secondary
motion are solved on each receiver. The native handshake version is now 2, so
all room participants need an updated build.

Deferred: robust arbitrary-proportion fitting, polished seated behavior, gesture
expressions/emotes, animation menus, FBT, eye/face tracking, OSC and
sender-solved body replication. VRM springs do not implement interactive
PhysBone grabbing/stretching or inter-avatar collisions.

Voice-driven visemes now run locally from capture and remotely from decoded
playout using Basis’s streaming OpenLipSync model. Alicia and Vita use their five
vowels plus conservative consonant mixtures; authored Oculus viseme shapes take
priority when present. See the [implementation and validation record](VISEME_PLAN.md).

## Vendored dependencies and assets

`project/addons/vrm` is Mainspring's Godot-VRM 2.0.1, subrepo reference
`651205484c35f5cd7ba56475ff636e10db8ad674`. `Godot-MToon-Shader` is its companion
3.4.0. `renik` contains Mainspring's GDScript helper, spine, limb and placement
implementation (Mainspring checkout `26ca25bff6618cd1a9a8cf004c652a634764f3fe`).
FPSloppa reference checkout: `a873edbc452c4982ceac48cf963374c4b9c986f7`.
Each dependency retains its own license. No native RenIK module
is required.

Local compatibility changes:

- Add `@tool` to VRM scripts used by the editor importer. Without this, Prim's
  Godot 4.7 import produced meshes without the normalized humanoid/spring setup.
- Retain FPSloppa's explicit successful `Error` returns in three VRM 1.0 extension
  callbacks (`VRMC_springBone`, MToon, HDR emissive multiplier).
- Bound spring delta to 50 ms and skip hidden/disabled runtime instances. Do not
  inherit FPSloppa's unconditional local-body spring disable.
- RenIK spine handles a root hips bone with no parent using the identity transform.
- Dangling-foot interpolation extracts rotation quaternions from scaled bases,
  avoiding normalized-basis errors after height calibration.

Both VRMs explicitly import with head-hiding `BothLayers`, first-person mask 2 and
third-person mask 4. Runtime remaps these to bits 17/18/19 for local first, local
third, and remote third. Preserve their `.vrm.import` files in source control.

Asset attribution and exact hashes are in
[`project/avatars/models/ATTRIBUTION.md`](../project/avatars/models/ATTRIBUTION.md).
The original VRM metadata and Mainspring sample license notice are retained.

## Validation

Automated checks on the patched Godot 4.7.2 build:

- `tests/avatars.gd`: Alicia/Vita import, normalized finger bones, finite solved
  bones, reachable standing head/wrist targets, independent springs, scaled
  instances, and synthetic OpenXR thumb/splay rotations through both skeletons.
  The standing fixture measured head errors below 3 cm and wrist errors below
  4 cm. These are fixture results, not a guarantee across motion/proportions.
- `tests/first_person.gd`: the desktop camera is the filtered camera; the central
  upper view matches the no-body image for both avatars, excluding a local head
  obstruction.
- `tests/menu.gd`: avatar selection, saved height, preview and desktop calibration
  explanation, alongside existing theater controls.
- `tests/protocol.gd`: round-trip joint rotations, masks and epochs; finite/range
  validation; sequence wrap and malformed packet rejection.
- Two-/six-process Linux integration and mixed Linux/Windows-under-Wine:
  avatar changes and full finger rotation payloads arrive while the same voice
  player continues advancing; video/voice synchronization and mute checks pass.
- Rust tests and Clippy pass. Windows native library cross-compiles and the packed
  Windows integration runs under Wine.

Run the Godot fixtures with the patched editor, `--path project --xr-mode off
--script res://tests/<fixture>.gd -- --desktop`. The visual fixtures require a
real rendering driver. Use `tools/test-integration.py` as described in
[building](BUILDING.md); set `PRIM_TEST_PEERS=5` for six processes.

The user tested single-player headset use successfully on 2026-09-13. The
reported scaled-basis log spam and horizontal preview mirroring are fixed.
Native Windows PCVR and multiplayer headset use still need a manual pass: Alicia preview, look down, reach/turn/crouch, controller
and hand tracking loss/recovery, recenter, and a remote observer. No FBT session is
part of that gate. Runtime robustness across many avatars remains future work.

## Private VRM comparison (local development only)

```sh
./run.sh --test-vrm /path/to/model.vrm             # VR
./run.sh --desktop --test-vrm /path/to/model.vrm   # desktop
```

The helper builds a disposable project under `.local/private-vrm-preview/project`,
copies the supplied model there, imports it with the same first/third-person layer
settings as the bundled avatars, and selects **Private VRM (local test)**. Open
**Avatar**, enable **Face close-up**, and unmute. **Vowels only (compare)** switches
between authored visemes and five-vowel approximations on that same model; leave
it off for the full set. Alicia and Vita remain available in the picker.

The preview has separate settings, no lobby secret and disabled networking. The
original VRM, normal catalog and normal export inputs are unchanged. Private model
copies and import caches remain under `.local` only; never distribute this scratch
project. Exit and run normally to return to the regular game. This development
helper is not the future user-model loading/distribution feature.

Verified locally with `hiibcot2_v6.vrm`: the importer retains all 15 authored
visemes and the comparison mode uses five authored vowels plus approximations.
`testsana.vrm` only has five vowels, so it cannot demonstrate full-set fidelity.
