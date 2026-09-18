# WayVR companion background cooperation

`0001-companion-background-owner.patch` is the small companion change applied to
`/home/s/code/wayvr` for Prim's Monado experiment. Existing unrelated changes in
that checkout were preserved. It is not part of Prim's automatic build/update.

Prim's OpenXR overlay uses placement **1**, below WayVR's **5**. Prim omits
cross-session depth submission, so WayVR's panels stay above the room. Session
ordering cannot put Prim between WayVR's skybox and its other layers, because
WayVR submits them together.

The patch polls libmonado at 2 Hz and skips only skybox rendering/submission while
an active, visible overlay with an allowed application name exists. The default
allowlist is `prim`. `WAYVR_BACKGROUND_OVERLAYS` overrides it with comma-separated
names; an empty value disables this cooperation. Other overlays are ignored.
Scene-app visibility continues to hide the skybox as before. Client disappearance
or failed status lookup restores the usual background policy. No focus, primary
role, input-blocking or persistent configuration changes are made.

The skybox allocation is retained while suppressed, allowing prompt restoration.
This is a local cooperation policy, not a standardized OpenXR background-owner
protocol; additional room providers should be explicitly allowlisted.

For another checkout, inspect `git apply --check` before applying the patch and
use that project's normal build environment. This checkout was built using the
Prim-pinned nixpkgs `wayvr` development inputs, plus Cargo/Rust, CMake, Ninja,
Python, shaderc, Vulkan headers and loader. The running debug service was restarted
without restarting Monado. The previous executable is preserved at
`.local/xr-companion/wayvr-before-companion` in Prim.

Live September 17 validation: WayVR logged background ownership true/false around
two physical Prim XR cycles; both were overlay clients, and Monado's PID stayed
unchanged. Panel composition and gesture comfort still need headset observation.
