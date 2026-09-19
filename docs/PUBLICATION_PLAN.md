# Public source preparation and validation

2026-09-19. The maintainer approved Unlicense for original code, publication of
missing dependency commits, a reviewed source snapshot and making the GitHub
repository public. The official signed application remains at
[prim.hiina.space](https://prim.hiina.space/); this work does not publish a new binary
release or alter its update feeds.

## Source snapshot

- Existing offline VRM preview/import work and its regression fixtures are recorded
  in `584077e`. It remains an offline experiment, not avatar distribution.
- Contributor docs, the previously local design plans and licensing are recorded
  in `e751e9f`. [AGENTS.md](../AGENTS.md) is 26 lines; detailed contracts/workflow are
  in [DEVELOPMENT.md](DEVELOPMENT.md), and conditional architecture context is in
  [DIRECTION.md](DIRECTION.md).
- The previously unavailable libmpv-zero commit
  `293d895d0e11eca1b3665a654fe4438e2d9ea031` was reviewed and published to that
  dependency's public `main`. The isolated checkout fetched it anonymously.
- `b3110b7` fixes the fresh-build header path: Nix appends `-dev` for the development
  output, so the output-link prefix must be `.local/mpv`, not `.local/mpv-dev`.
  Both `build.sh` and the manual build recipe now use the correct prefix.
- [UNLICENSE](../UNLICENSE) covers original material. [LICENSES.md](../LICENSES.md)
  identifies third-party/asset exceptions and the Apache-2.0 viseme adaptations;
  native crate metadata is `Unlicense AND Apache-2.0`. See the
  [licensing record](LICENSING_PLAN.md) for binary-distribution responsibilities.

## Validation

Tested source/runtime revision: `b3110b7`. Subsequent preparation edits are help
text and documentation, including these results.

- Cloned the candidate without local Git object sharing, then initialized all
  submodules through their public HTTPS URLs with global/system Git configuration
  disabled. OpenVR's separate pinned repository was also fetched by the build.
- Ran the documented `./build.sh` with four build jobs in the isolated checkout.
  It had no borrowed `.local`, `target`, imported assets or lobby configuration.
  Normal host Nix/store and dependency-download caches were available; this was
  not a cold build of every dependency or a source build on a Nix-free machine.
- The first attempt compiled Rust, then exposed the output-link bug above. After
  fixing it, one retry was interrupted by temporary disk pressure from an unrelated
  optional test-tool evaluation. Nix cleaned up that failed temporary copy. The
  resumed build completed, including C++ extensions, runtime/tool fetching and
  Godot/VRM import, with no script/import errors. No staged outputs were copied in
  from the maintainer's original checkout.
- Native tests: **12 passed, 1 public-DHT test intentionally ignored**.
- Offline raw-VRM import fixture: **passed** for the bundled models, including
  repeated instances, spring initialization, finite poses and invalid-file rejection.
- Rendered two-process integration on a private X display: **passed** with a client
  providing a shared MKV to the host, synchronized playback, voice, avatars,
  pause/seek/resume, mute and main-thread stall coverage. Both peers reported no
  failures; no script/engine errors appeared in their logs. Tests used synthetic
  audio, isolated preferences and a local-only test room.
- Full-app `--headless` startup exits zero but reports a missing rendering device;
  it is **not** a passing media smoke. The helper's example and build documentation
  now require a rendered display for the normal application. Headless import and
  selected script fixtures remain useful separate checks.
- Exported a disposable Linux bundle/test PCK from the fresh build and ran it on
  the Ubuntu test machine with `/nix` hidden. **All eight packaged-media checks
  passed**: load, advancing playback, video dimensions, changing rendered frames,
  nonfinite-speed rejection, accepted tempo correction, observed tempo and pause.
  A captured frame was visually checked. This used software Vulkan (`llvmpipe`),
  not hardware GPU/headset validation. Locale/Compose/XIM warnings were present;
  no script errors or missing-rendering-device error occurred. No official package
  or update feed was replaced.

Local evidence is retained under the maintainer's ignored `.local/publication/`
(build attempts, native/script/integration logs and audit summaries). Those logs
are not required to build or contribute and are not part of the public source.

## Publication review

- Scanned 558 reachable Prim history blobs with high-confidence private-key,
  GitHub/AWS token patterns and the exact ignored local lobby value: no matches.
  The dependency history was also checked. A URL-credential example was reviewed
  separately and is an intentional launcher redaction fixture. This bounded scan
  is not a guarantee against every possible kind of sensitive data.
- Kept history intact. Private tracker prerequisites were replaced with in-repo
  rationale; live tests use reader-controlled server placeholders. Production
  operations are marked as maintainer history and signing-key examples are
  parameterized. Harmless historical machine paths are not treated as credentials.
- A repository-only fresh-agent review found the 26-line guidance clear and the
  contribution policy welcoming. It correctly distinguished useful end-to-end FBT
  and focused fixes from architecture experiments whose assumptions need review.
- Repository documentation links were checked against tracked files, including
  the previously untracked plans. Remaining platform/device gaps are explicit.

## Remaining scope

Windows source builds still use the documented multi-stage cross-build recipe;
there is no one-command native Windows build. Native Windows headset/overlay,
other controller profiles, WAN/relay behavior, cold build cost and broader OS
coverage are not established by this source-publication pass. Contributors can
help reproduce and improve those workflows.

The initial shared lobby remains friends-scale. Source builds generate an
isolated room by default; coordinated manual testing can use an intentionally
shared configuration. This is not a promise of private admission or moderation.
No general cross-version/fork compatibility promise or CI requirement was added.
