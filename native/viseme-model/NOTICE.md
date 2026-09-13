# OpenLipSync provenance

Model, configuration, and the ported signal-processing design come from
BasisVR/Basis `a59efeb2ec20938702b97c42b0a08ffd860eba3b`, package
`Basis/Packages/com.basis.openlipsync` (0.2.0), derived from KyuubiYoru/OpenLipSync.
Source: https://github.com/BasisVR/Basis/tree/a59efeb2ec20938702b97c42b0a08ffd860eba3b/Basis/Packages/com.basis.openlipsync

Model SHA-256: `02f2dd6825652fd3d86f7777825cc0af7d8335f064984eeeceb42540fe7d9804`.
Retain LICENSE (Apache-2.0) and THIRD_PARTY_NOTICES.md with this directory.
The model is trained with LibriSpeech train-clean-100; the original notice
contains its CC BY 4.0 attribution. The notice's 128-channel architecture text
is historical: config.json and the pinned graph describe the 256-channel model.

Prim's `native/src/viseme/frontend.rs` and `model.rs` are a Rust adaptation,
2026-09-13, retaining the sample geometry, HTK filters, raw-dB normalization
contract, 48-tap resampling filter and streaming cache layout. The resampler
uses an exact rational sample clock to eliminate chunk-dependent phase drift.
The worker uses bounded audio queues and owns all inference outside Godot's
mix/render threads. ORT owns intermediate output allocations on that worker.

The CATS Blender plugin's viseme generation was consulted as a technique:
https://github.com/absolute-quantum/cats-blender-plugin/blob/master/tools/viseme.py
Prim's expression mapper uses independently chosen, conservative vowel mixtures;
it does not include Blender code or generated meshes. Authored `vrc.v_*` shapes
have priority. These mixtures cannot reconstruct absent tongue/lip geometry.

Private evaluation avatars are not part of this directory or any package.
