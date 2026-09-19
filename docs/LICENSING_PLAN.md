# Source and distribution licensing

Adopted, 2026-09-19: **Unlicense for original Prim code**,
with explicit exceptions for third-party and derived material, and the required
licenses/notices/corresponding source for the combined application. This document
records the decision and remaining binary-distribution work. The root
[UNLICENSE](../UNLICENSE) and [exceptions index](../LICENSES.md) state the terms;
native package metadata accounts for its Apache-2.0 adaptations/model too.

The maintainer prefers Unlicense or WTFPL and accepts dependency-required
copyleft. Unlicense matches that preference and is already used by the
`godot-libmpv-zero` wrapper; `godot-network-audio` has WTFPL text. Switching Prim's
own code to MIT/Apache would not remove copyleft obligations from dependencies.
The FSF lists Unlicense as GPL-compatible; compatibility permits combination,
not removal of the combined work's GPL requirements.
[Unlicense text](https://unlicense.org/),
[FSF license classification](https://www.gnu.org/licenses/license-list.html.en#Unlicense).

## What the checked source tells us

| Material | Evidence / treatment |
| --- | --- |
| Original Prim source | Unlicense, with a root license and exceptions index. `Cargo.toml` uses `Unlicense AND Apache-2.0` for the native crate's original code and embedded/adapted viseme material. |
| libmpv wrapper and audio submodule | Their own license files remain authoritative; do not relabel their vendored dependencies. |
| Patched mpv and codec dependency graph | The pinned mpv source defaults to GPL, with a separate reduced LGPL build option. Evaluated Linux Meson flags do not disable GPL and enable GPL-only features. The documented Windows recipe also does not disable it. Treat current packages as containing GPL components, not as an LGPL-only build. Audit the actual link/runtime set before settling the combined-distribution license/version. |
| Viseme model and Rust adaptation | `native/viseme-model/NOTICE.md` identifies Apache-2.0 material and names `native/src/viseme/frontend.rs` and `model.rs` as adaptations. Both files now carry explicit Apache-2.0 identifiers and retain the provenance/notice. |
| Godot, VRM/MToon/RenIK, OpenVR and runtimes | Preserve each dependency's license and attribution, including modifications and separately bundled runtime libraries. |
| Alicia and Vita avatars | Keep [asset attribution](../project/avatars/models/ATTRIBUTION.md) separate from code licensing. Exact hashes and embedded license links were checked. Alicia retains its special character/model terms; Vita is described using its embedded redistribution/modification permission, not the broader historical CC0 claim. |

mpv's pinned [copyright notice](https://github.com/hiinaspace/mpv/blob/970250ad1480e78ca03999e1733d212278f07dbb/Copyright)
explains that its build option alone does not settle every file or dependency's
license. Its [Meson options](https://github.com/hiinaspace/mpv/blob/970250ad1480e78ca03999e1733d212278f07dbb/meson.options)
default `gpl` to true. Dynamic library loading is not a sufficient basis for
assuming those distribution obligations disappear. No runtime refactor or codec
removal is proposed merely to choose an original-source license.

## Source decision and remaining distribution work

1. The source snapshot preserves separate code/asset notices, explicitly identifies
   the viseme adaptations, and documents exact avatar provenance/embedded terms.
2. The root original-source license, native package metadata, exceptions index and
   contribution terms now agree. Preserve that separation in future changes.
3. Identify the actual Linux/Windows GPL/LGPL components and compatible terms for
   the combined executable. Preserve source/patch/build-recipe availability for
   the exact distributed versions and the notices required by other dependencies.
4. Verify the source package against the actual binary build inputs.
   `tools/package-support.py` already archives tracked `HEAD` sources and submodule
   revisions, but it is not proof that uncommitted changes used in a binary are in
   that archive. Nor is a list of upstream URLs a complete corresponding-source
   check. Record exact sources, local patches and build instructions per release.

This is a targeted publication task, not a reason to replace working libraries
or choose a heavier license for independently reusable original code.
