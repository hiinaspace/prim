# Prim OpenVR peek prototype

Upstream: https://github.com/hiinaspace/godot-openvr-overlay at
`c766e97e5173276b3d90bc3b38f8bd76a2de00d7`, with its pinned godot-cpp submodule.
`tools/build-openvr-overlay.sh [linux|windows]` creates a revision/patch-keyed
source cache under `.local`, applies `0001-prim-lifecycle.patch`, builds the
restricted class profile, and stages the extension. `build.sh` includes Linux;
Windows uses the engine cross toolchain described in `docs/BUILDING.md` (or
`MINGW_PREFIX`). OpenVR SDK comes from the pinned video dependency's nixpkgs.

The patch supplies Prim's head tracker, updates actions before reading poses,
extracts bindings from a packed game into `user://openvr-input`, handles runtime
quit without terminating the room, and hides overlays independently of scene
rendering. Prim owns per-process overlay keys and the orderly provider handoff.
The shipped action JSON lives in `project/addons/godot-openvr-overlay`; its
Index/Knuckles grip path is `/input/grip`, not the rejected `/input/squeeze`.
Upstream and godot-cpp notices are alongside those assets.

The prototype retains upstream's two Vulkan projective eye overlays: direct
left-eye array submission, right-eye copy. It is opt-in through **Comfort →
Experimental SteamVR peek**, with the tracking-only helper still available.
It never claims a scene slot alongside a game. Game exit returns to the ordinary
OpenXR Prim scene; a new game returns Prim to its OpenVR overlay.

Local Linux SteamVR 2.17.10 / Beyond / Index qualification: both hands and remote
pose delivery, repeated show/hide, game → Prim scene → game handoffs, retained
voice/media/session objects; user saw normal stereo and working lift/hide plus
wrist dwell toggles. Linux and Windows DLL builds pass. This is not Windows
headset, other-controller, latency, or foreground-game performance qualification.
Zero-prediction sampling and right-eye copy remain prototype timing limitations.
No mono quad fallback was selected after the positive stereo check.

Use `tools/test-steamvr-companion.py --allow-scene-takeover --peek` with SteamVR
running and an active headset/controllers. `PRIM_PEEK_INTERACTIVE=1` adds a
2-minute gesture/wrist check. The test uses a rendering OpenXR reference game
with a distinct executable name; a non-rendering ownership probe remains stuck
on SteamVR's Next up screen, and shared executable identities can mix bindings.
