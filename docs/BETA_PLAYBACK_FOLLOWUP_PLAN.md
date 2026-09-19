# Friends beta playback and sharing follow-up

Planned and implemented 2026-09-15 against Prim `015ce98`. The original
sequence below records the design; the implementation and verification record
at the end describes what shipped into the local working tree.

## Feedback and scope

- Direct video streaming worked well in the user's latest-build session.
- Allow every connected participant to share a local video and replace the
  current room source, without a host approval prompt.
- Add a local direct-stereo movie-audio mode for comparison with the current
  spatial speakers, plus local embedded-subtitle selection.
- Make file selection discoverable. The user confirmed that a native desktop
  picker and desktop-window drag-and-drop are sufficient for this pass.
- Investigate intermittent Alicia springbone resets. An affected wearer and
  a remote viewer both saw them; another Alicia wearer remained stable.
  They may also occur during straight/physical playspace movement, so snap
  turning alone is not an established explanation.
- Design the sharing boundary to support on-demand personal VRM downloads in
  a later pass. Implementing avatar downloads is outside this first increment.

## Recommended sequence

1. **Local playback usability:** direct stereo, embedded-subtitle selection,
   and explicit Browse/drop selection, as separately reviewable changes.
   Begin a bounded springbone reproduction/diagnostic pass here.
2. **Any-participant video sharing:** separate file ownership from playback
   authority, then validate replacement, cancellation and disconnect behavior.
3. **Springbone correction:** land the correction when a reproducer identifies
   the trigger; it can accompany either increment. An intermittent visual bug
   should not block the independent playback/sharing work indefinitely.
4. **Later personal avatars:** prove runtime VRM import first, then add bounded
   object downloads and integrate avatar configuration.

This order provides a useful listening comparison and easier test-file loading
before changing the networking lifecycle. Define the provider/authority contract
below before implementation, without first building a generic asset platform.

## 1. Local playback controls

### Movie audio

Expose a saved local choice: **Screen speakers / Direct stereo**. Keep the
current default initially. Direct stereo preserves left/right channel identity,
bypasses movie HRTF and distance filtering/falloff, and uses the existing Movie
bus volume. Voice keeps its existing spatial behavior.

Current evidence: `project/main.gd` routes two Steam Audio speakers through the
Movie bus; libmpv is already configured to downmix to stereo. However,
`AudioBridge::pull_channel_frames()` duplicates each individual channel into
both output ears. Attaching those streams to ordinary players alone would not
produce the requested stereo output.

Add a paired stereo stream/consumer at the libmpv-zero bridge, with an atomic
handoff between output modes. Preserve playback timing and bounded queues;
the inactive output must not also consume the same audio. Handle switches
during playback, pause, seek and source replacement without reloading the file.

Validate channel isolation, identical-channel/impulse alignment, surround
downmix, volume, and repeated switches using recorded output. Then compare
listening in the headset. The toggle is a diagnostic and user preference;
it is not proof that the underlying spatial-audio complaint is fixed.

### Embedded subtitles

Add a local menu populated from available subtitle tracks, with **Off** and
labels containing language/title where present, plus a stable fallback label.
Reflect mpv's current choice after load; clear obsolete entries on replacement
and refresh when tracks change. Retain ordinary automatic selection for a new
source initially rather than persisting a numerical track ID across files.

Expose cached track metadata and asynchronous selection through the existing
mpv event loop. mpv supplies `track-list` and `sid`; preserve its rendering of
ASS styling and bitmap subtitles. Selection must not generate room playback
requests, seek, or reload the media. External subtitle discovery, styling menus,
and dual-subtitle rendering can follow separately.

Validate a multi-track MKV: select each track and Off during playback and pause,
seek, replace with a file without subtitles, and have two peers choose different
tracks on the same shared file. Inspect the rendered result as well as metadata.

Reference: [mpv track properties](https://mpv.io/manual/stable/#property-list).

### File selection

The current Sharing tab already constructs a native `FileDialog`, but only
when **Share file** is pressed with an empty path. Replace that hidden behavior
with explicit **Browse…**, a selected filename/path, and **Share with room**.
Accept one file dropped onto the main desktop window, open/populate the Sharing
tab, and use the same Share action. Browse/drop select the file; sharing commits
the room replacement. Multiple files receive a clear single-file message.

Keep pasted paths as a fallback. Normalize surrounding quotes and valid local
`file://` URIs, preserving Unicode and spaces; reject malformed or ambiguous
input without shell interpretation. Handle Windows drive paths and URI escaping
explicitly. Opening/canceling a picker must preserve the running room/media.

Godot provides [native file dialogs](https://docs.godotengine.org/en/stable/classes/class_filedialog.html#class-filedialog-property-use-native-dialog)
and [main-window file drops](https://docs.godotengine.org/en/stable/classes/class_window.html#class-window-signal-files-dropped).
Connect the drop signal on the native main window, not the world-menu SubViewport.
Show a short indication in VR that the picker opened on the desktop.

Explorer's ordinary Copy uses a file-list clipboard representation such as
[CF_HDROP](https://learn.microsoft.com/en-us/windows/win32/shell/clipboard), while
[Godot's clipboard_get](https://docs.godotengine.org/en/stable/classes/class_displayserver.html#class-displayserver-method-clipboard-get)
returns text when available. Treat native file-list clipboard extraction as
optional later polish; it is unnecessary for the accepted picker/drop scope.
Wine can exercise parts of the Windows package, but actual Explorer clipboard,
Windows desktop drag/drop and VR focus behavior need native checks.

## 2. Any participant can supply the video

### Authority and user behavior

Keep the room host ordering source changes, pause/seek requests and playback
clock snapshots. The **provider** is whichever participant supplies the bytes.
Every room member can publish their own source; an accepted Share implicitly
replaces the current URL/file. This matches existing client URL replacement,
which already goes through a host-ordered request.

Proposed defaults:

- Prepare and validate the selected local file before requesting replacement.
  A picker cancellation or invalid path leaves current playback intact.
- The host serializes competing offers. Each accepted replacement has a new
  generation; stale completion/stop messages cannot replace or stop a newer one.
- Viewers, including the room host, fetch directly from the provider. The
  provider plays its local file and follows the room clock like another viewer.
- The provider can stop its own active share; the room host retains stop control.
  Any participant can still replace the source through Share/Open.
- If the provider leaves or its source changes/disappears, stop that room source
  with a clear message. Do not silently select an older video.
- The provider owns its upload limit and per-viewer/per-share relay consent,
  including consent for the room host when it needs a relay. Handle host loading
  and relay-wait states explicitly before starting synchronized playback.

### Required code changes and limits

Host-only checks exist in `project/media/playback.gd`, `world_menu.gd`,
`native/src/session.rs`, and `native/src/media/mod.rs`; this is more than enabling
a button. The current Media object also uses one lifecycle for serving and
receiving: both `share()` and `receive()` call `stop()`.

Split local publication, incoming playback transfer, and committed room-source
state so preparing a share cannot tear down the source currently being watched.
Correlate prepare/accept/activate/cancel events with offer and source generations.
Retain a single active room video; revoke the previous publication when replaced.

Replace host-only file ownership with authenticated provider ownership:

- Bind a submitted descriptor's owner to the actual connected sender.
- Accept room source announcements only from the current room host.
- Fetch only from the authenticated, connected provider authorized by that
  announcement; validate descriptor fields and transfer identifiers.
- Preserve room proof, byte/range bounds, temporary-cache bounds, file mutation
  checks, and revocation on replacement, cancellation or membership loss.
- Keep local paths local and media bytes on separate Iroh streams. Do not route
  bulk media through room control or voice/pose channels.

This is a reasonable extension of the private-friends model: room members
already have shared playback control. Membership authentication and ownership
checks still matter even when there is no host approval prompt. Arbitrary VRM
parsing is a separate boundary addressed in the later increment.

Validate with a host plus two clients, then the existing six-person setup:
non-host sharing, provider-local playback, host receiving, late join, seek/pause,
client-to-client replacement, URL/file transitions, concurrent offers, late
completion/stop events, invalid offers, provider departure, file mutation,
revoked requests and relay decisions owned by the provider. Exercise host changes
under the existing room lifecycle. Include a Windows provider and receiver;
keep controlled relay-gate checks distinct from actual WAN relay/fallback tests.

## 3. Springbone investigation

Static evidence gives several explicit resets:

- `main.gd`: a frame longer than 250 ms, recenter, and snap turn reset the local
  driver and increment a reset epoch sent to other peers.
- `avatars/driver.gd`: an uninitialized pose or a head jump over one meter resets
  placement and reconstructs spring state during the physics update.
- `net/avatar.gd`: reset-epoch changes, visibility/tracking recovery and proximity
  hiding/reappearance can reset the remote driver.
- The VRM secondary implementation can rebuild internal state when spring or
  collider configuration changes. Its runtime step already caps delta at 50 ms.

The reported agreement between wearer and viewer prioritizes sender-originated
events, pose discontinuities, and shared solver behavior. It does not establish
which reset path fires, and straight movement remains part of the reproduction.

Add bounded opt-in diagnostics recording reset reason, avatar/peer, frame delta,
tracking transitions, reset epoch and pose displacement. Distinguish actual
reset requests from internal spring reinitialization. Replay Alicia motion with
physical/smooth movement, snap/smooth turning, one-client stalls, tracking loss,
different height scales, and local/remote views. Check reset timing against the
IK/modifier order so springs initialize from the correct solved pose.

For ordinary movement and recoverable frame stalls, aim to preserve spring
state. For a coordinate discontinuity, evaluate transforming spring history
with the avatar root; use a full reset when history is invalid. Choose the fix
from the reproducer instead of globally disabling springs or raising thresholds.
Require local and remote visual comparison in addition to finite-bone tests.

## 4. Later on-demand VRM avatars

The current catalog preloads two scenes imported at build time. Downloading a
`.vrm` does not make it loadable through that catalog in an exported game.
First prove runtime import of a locally selected VRM using the bundled GLTF/VRM
extensions, humanoid normalization, first-person layers, materials, expressions
and springbones. Check required bones/proportions and preserve fallback avatars.

Then extend transfers to named objects with independent lifetimes:

- Reuse endpoint identity, room authorization, Iroh streams, provider ownership,
  cancellation, transfer bounds and consent policy from video sharing.
- Give avatars a content hash, byte size, format and avatar revision; keep the
  content identity separate from the revocable transfer capability.
- Allow one movie stream and several bounded avatar transfers simultaneously.
  Avatar transfer progress must not replace/stop the room video or starve voice.
- Download complete VRMs to a bounded temporary cache, verify size/hash before
  import, deduplicate in-flight requests, and ignore completion for stale avatar
  revisions. Load a fallback until ready; changing avatars revokes obsolete work.
- Validate as VRM/glTF data with bounded file, texture, geometry, bone and spring
  resources. Reject external file/network references and never accept scripts,
  Godot scenes or PCKs as peer avatars. A hash verifies content identity, not safety.
- Begin with owner-to-viewer delivery; persistent caches and peer redistribution
  need separate decisions. Reassess iroh-blobs if verified storage/reuse becomes
  useful rather than migrating the working video transport preemptively.

Before implementing that later pass, settle supported VRM versions, concrete
resource/cache limits and avatar-download/relay preferences. Those choices do
not block the current video work.

## Implementation and verification record (2026-09-15)

Implemented in the working tree:

- Any connected participant can prepare and offer a local movie. The room host
  orders activation and source snapshots; the selected provider serves bytes.
  Preparing or canceling another offer preserves current playback. Stale offers,
  readiness messages and stops cannot replace a newer source. Provider departure
  clears the source. Existing member authentication, bounded range cache and
  provider-controlled relay consent remain in place.
- Local direct stereo output, embedded-subtitle track selection (including Off),
  native Browse, explicit Share, path normalization, and desktop file drops.
  Choosing/dropping a file does not share it until Share is pressed. Comfort
  controls have their own tab so the theater controls fit the world menu.
- Pulled FPSloppa cleanly to `b0fc725`. Ported its actual-bone-index spring
  initialization correction and missing-parent/bone guards. Added opt-in,
  rate-limited reset-reason telemetry (`PRIM_TRACE_SPRINGS=1`). The intermittent
  human-reported snapping is not established as fixed; no speculative motion
  threshold changes were made.
- Raw runtime VRM loading and offline avatar preview via
  `./run.sh --desktop --runtime-vrm /absolute/path/model.vrm`. Preview blocks
  joining until a bundled avatar is selected. Alicia and Vita VRM 0.x pass;
  VRM 1.0 registration is present but has no tested fixture. Peer avatar
  downloading and its full asset resource policy remain a later pass. Runtime
  instances duplicate a live imported template, avoiding an exported-build
  PackedScene typed-array remapping error found in final smoke testing.

Protocol hello is now **4**: update every participant together.

Completed checks:

| Check | Result |
| --- | --- |
| Native Rust tests | 9 passed; public DHT test intentionally ignored |
| Clippy all targets, warnings denied | Passed |
| Linux and Windows native/video builds | Passed |
| Generated audio/subtitle controls on Linux and Wine | Passed: distinct L/R tones, identical channels, center-only 5.1 downmix, output switching, pause/seek/volume, rendered local subtitle selection and Off |
| Menu and path/drop tests | Passed; sharing-menu screenshot inspected |
| Three- and six-participant nonhost-provider integration | Passed |
| Late join, competing offers, stale/spoofed messages, failed preparation, stop/replacement/departure | Passed |
| Mixed Linux/Wine with Windows provider and Windows receiver | Both passed |
| Existing avatar suite and raw runtime VRM motion/rig tests | Passed |
| Raw VRM import without editor sidecars in exported Linux and Windows/Wine packages | Passed |

Detailed local logs are `.local/beta-*.log`; these are test artifacts, not
committed fixtures. Local packages are `.local/beta-linux` and
`.local/beta-windows`. No release was published and no changes were committed.

Remaining manual gates: headset listening for the original stereo/phasing
complaint, springbone observation with reset telemetry, actual native file
picker and OS drag/drop behavior, and native Windows/PCVR. Wine establishes
packaged Windows execution only. Real WAN relay and ordinary-Linux execution
with `/nix` hidden were not repeated in this pass. Controlled relay-consent tests
passed. Full avatar sharing remains deliberately deferred.
