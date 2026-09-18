# Movie speaker coloration — September 17, 2026

The user confirmed repeated physical-headset desktop/VR switching now works, then
reported movie audio sounding dull through Screen speakers compared with Direct
stereo. The movie speakers already disable Godot's attenuation high shelf and
Steam Audio air absorption. Reflections/directivity are off. Occlusion is enabled
but the bypass measurement below does not show a material spectral loss from it.

## Measurement and cause

`tools/test-spatial-audio.py` generates a deterministic white-noise video and
plays it through real mpv, the audio bridge, Steam Audio and the Godot master bus.
It records float PCM in an isolated desktop process with a private PulseAudio
sink and profile. Measurement is **before PipeWire/device processing**, which is
sufficient to reproduce the coloration inside Prim. It does not measure headset
frequency response or subjective comfort.

The plugin's original direct sound uses first-order Ambisonics encode then
binaural decode. A single movie speaker shows roughly 7–10 dB lower 1–6 kHz
energy relative to its 200–1000 Hz band. Bypassing that stage is approximately
flat in the same bands; two-speaker interference cannot explain a single-speaker
loss. This isolates the broad coloration to that spatialization path, rather
than the disabled distance filters. It does not prove that every notch or timing
artifact has the same cause.

## Why first order can sound dull

The plugin encodes a point source into only four first-order spherical-harmonic
channels, then decodes these with HRTF filters. In Steam Audio 4.8.1,
[`AmbisonicsBinauralEffect::apply`](https://github.com/ValveSoftware/steam-audio/blob/v4.8.1/core/src/core/ambisonics_binaural_effect.cpp)
convolves each coefficient with an Ambisonics HRTF and sums the results. Its
order-dependent weighting leaves the omnidirectional term at 1 and scales the
first-order terms by about 0.575. This is angular weighting, not an explicit
frequency low-pass filter.

The [HRTF projection code](https://github.com/ValveSoftware/steam-audio/blob/v4.8.1/core/src/core/hrtf_database.cpp)
constructs Ambisonics filters by weighted sums of complex directional HRTFs.
**Mechanistic interpretation:** restricting this representation to first order
smooths fine angular structure; phase-dependent summation can cancel energy,
particularly at higher frequencies where directional responses vary rapidly.
The measured response establishes the coloration of this particular path; it
does not establish a universal first-order cutoff or separate the contributions
of truncation, weighting, and the supplied HRTF dataset. A point-source renderer
can use the actual source direction without that intermediate sound-field
approximation. Source polar/directivity attenuation is a separate feature.

## Candidate

The opt-in `point_source_binaural` property uses Steam Audio's
[point-source binaural effect](https://valvesoftware.github.io/steam-audio/doc/capi/binaural-effect.html)
with listener-relative direction, bilinear HRTF interpolation and full spatial
blend. It bypasses the first-order encode/decode round trip for direct sound;
existing reflection processing is retained. Prim enables it on the two movie
speakers and peer voices, as requested. Other plugin users retain their existing
defaults. Source directivity remains disabled.

Patch: `dependencies/godot-libmpv-zero/patches/steam-audio/0005-point-source-binaural.patch`.
The same single-speaker test now measures about +0.4 to +0.8 dB in the 1–6 kHz
bands relative to the low band. HRTF spectral notches remain (notably 6–10 kHz in
this direction); flattening these with an arbitrary EQ would change localization.
Identical audio from two spatial sources can additionally interfere at each ear;
the single-source comparison deliberately excludes that confound.

The automated regression checks the spectral improvement, bypass flatness,
left/right ear dominance and a 90-degree listener turn. The existing 29 playback
control checks also pass with the rebuilt plugin. Listening acceptance is pending.

Run:

```sh
nix develop --command uv run --with numpy python tools/test-spatial-audio.py
```

Results and PCM live under `.local/spatial-audio/<timestamp>/`; the first completed
candidate run was `20260917-154807`; the final spectrum/alignment run is
`20260917-155457` (both split captures have normalized difference 0.0). Initial diagnostic captures/build logs are in
`.local/audio-coloration/`. The staged plugin is from
`/nix/store/0zlxvazq99j2pri42chb4094r6fj2rfx-godot-steam-audio-8f65c29-sdk-4.8.1`.
`./build.sh` now always resolves the Steam Audio derivation, and
`./tools/build-steam-audio.sh` can resolve it alone. Restart Prim to load the new
plugin; already-running processes retain the old mapped library. Existing beta
packages and the Windows plugin were not rebuilt.

Human follow-up: compare Direct stereo and Screen speakers on familiar dialogue
and music, adjusting overall gain when judging timbre. Walk and turn around the
room; report remaining dullness separately from direction-dependent notches or
changes after output switching. This is a spectral correction candidate, not a
claim that the two paths should sound identical.

## Split-channel startup timing

The two unspatialized speaker-bus captures also exposed a separate offset: about
118 output samples at 44.1 kHz (2.68 ms, consistent with a 128-sample source
prefill at 48 kHz). Godot's `begin_resample()` consumes input during `start()`.
The audio-server lock excludes mixing but does not stop mpv's producer: one
new channel could prefill with real PCM and the other with silence. The bridge
now suppresses PCM consumption while the target pair is being configured, giving
both resamplers the same silent prefill. The final corrected captures had exactly
zero difference between channels, including a second
Direct-stereo -> split-speaker transition. This fixes the reproduced startup
race, not every possible future underrun or natural HRTF interference.

Peer voices additionally disable Godot's attenuation high shelf explicitly.
`AudioStreamPlayer3D::_get_attenuation_db()` includes the player's volume even
with distance attenuation disabled, and uses that result to scale the shelf.
Thus a quiet voice could previously receive extra filtering even without
Godot distance falloff. Movie speakers already disabled this shelf.

Diagnostic limitation: an extra `get_audio_diagnostics()` trace call stalled one
probe (`.local/spatial-audio/20260917-155206`); the bounded runner killed its own
process and cleaned up. It is not used in the application or the final spectral
harness. A separate snapshot/lock-order investigation is pending; the alignment
results above come from captured PCM rather than that getter.


## Final runtime boundary

The final spectrum/alignment run `20260917-155457` and 29 playback controls passed
on the staged audio builds. A concurrent XR loss/recovery run
`.local/xr-lifecycle-tests/20260917-155458` reached the intentional runtime-loss
phase, but at 15:55:15 NVIDIA logged Xid 51 for normal Monado PID 75109, then Xid
154 and `NV_ERR_RESET_REQUIRED`. Private runtime recovery failed to create a
Vulkan device. A subsequent isolated run `20260917-155610` could not initialize
Vulkan at startup. Causality beyond this timing is not established. No GPU reset,
normal-service restart or desktop restart was attempted. Test-owned processes and
sinks were cleaned up. Driver recovery is required before headset retesting;
the final XR rerun is **not passed**. Avoid repeating runtime-loss injection on
this live GPU until that failure mode is investigated.
