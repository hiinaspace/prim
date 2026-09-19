# Licensing and attribution

Original Prim code and documentation are released under the [Unlicense](UNLICENSE).
This grant covers material the Prim authors own; it does not relicense the
third-party code, adaptations, assets or dependencies listed below. Existing
file-level notices take precedence for their material.

New original contributions use the Unlicense. Changes to third-party or derived
files retain those files' applicable terms and attribution. Identify copied or
adapted material and include its notice with a contribution.

## Material with separate terms

| Material | Terms and retained notices |
| --- | --- |
| `native/src/viseme/frontend.rs`, `native/src/viseme/model.rs`, and `native/viseme-model/` | Apache-2.0 OpenLipSync adaptations/model; [license](native/viseme-model/LICENSE), [provenance and modification notice](native/viseme-model/NOTICE.md), [third-party notices](native/viseme-model/THIRD_PARTY_NOTICES.md). The native crate's SPDX expression is `Unlicense AND Apache-2.0` because it contains both kinds of material. |
| `project/addons/vrm/` | MIT with retained upstream copyrights; [license](project/addons/vrm/LICENSE). Sample assets have their own terms. |
| `project/addons/Godot-MToon-Shader/` | MIT; [license](project/addons/Godot-MToon-Shader/LICENSE). |
| `project/addons/renik/` | MIT; [license](project/addons/renik/LICENSE.txt). |
| OpenVR overlay extension/assets | Unlicense for the upstream overlay code, with MIT godot-cpp material; [overlay license](project/addons/godot-openvr-overlay/LICENSE.md), [godot-cpp license](project/addons/godot-openvr-overlay/LICENSE.godot-cpp.txt), [source pin and modifications](build-support/openvr-overlay/README.md). OpenVR SDK has its own notice, staged by the helper build. |
| Alicia Solid and Vita VRMs | Separate model terms, **not Unlicense or a blanket CC0 grant**. See [exact-file attribution and license links](project/avatars/models/ATTRIBUTION.md) and [sample notice](project/avatars/models/LICENSE_SAMPLES.txt). Alicia has special use restrictions; Vita's embedded license URL permits redistribution/modification. |
| `dependencies/godot-libmpv-zero/` | Wrapper under [Unlicense](dependencies/godot-libmpv-zero/LICENSE.md); its dependencies and patches retain their respective licenses. |
| `dependencies/godot-network-audio/` | [WTFPL](dependencies/godot-network-audio/LICENSE); dependency licenses still apply. |
| Engine, Steam Audio, WayVR and other upstream patches | Patches modifying upstream material retain that material's license. Source pins/build recipes are under `build-support/` and the video dependency's `nix/` and `patches/` directories. |

## Built distributions

The root Unlicense is not a license for every component of a compiled package.
The current libmpv build enables GPL components, and codec/runtime libraries have
additional terms. Preserve their notices and satisfy the applicable source and
redistribution requirements for the combined build. Original Prim files remain
separately available under the terms above.

Pinned dependencies are recorded in `Cargo.lock`, `flake.lock`, the submodule
gitlinks, `build-support/` manifests and launcher package lockfiles. Packaging
collects notices and source references; maintainers must match corresponding
source, modifications and build recipes to the binaries actually distributed.
See [licensing decisions and distribution follow-up](docs/LICENSING_PLAN.md).
