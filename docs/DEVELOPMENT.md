# Development workflow and contracts

Read the sections relevant to your change. Contribution scope and experimental
PR policy are in [CONTRIBUTING.md](../CONTRIBUTING.md); the product and architecture
direction is in [DIRECTION.md](DIRECTION.md).

## Preserve the important contracts

- Keep room membership, playback coordination, file-provider ownership, and XR
  presentation distinct. A provider need not be the room host.
- Keep voice and pose responsive. Media bytes use separate bounded transfers;
  they do not belong in room-control messages or voice/pose datagrams.
- Preserve explicit microphone choices. Deafen affects received Prim voice/media,
  not microphone transmission or shared playback.
- Preserve desktop and headset-and-hands fallbacks. Extra trackers, another XR
  runtime, or an absent optional helper must not silently break those modes.
- Preserve peer authentication, input bounds, source revocation, and per-share
  relay consent. Discovery is not admission. See [protocol](../docs/PROTOCOL.md).
- Keep durable content separate from clocks, presence, tracking samples, live
  coordinator state, and temporary media transfers in future persistence work.
- Do not treat local/offline VRM import as a network content-loading contract.

## Repository map and workflow

- `project/`: Godot room, UI, avatars, media policy, XR lifecycle, script fixtures.
- `native/src/`: Rust session/network/media/viseme integration.
- `dependencies/`: pinned source submodules; read their instructions when editing.
- `build-support/`, `tools/`: engine patches, native staging, packaging and tests.
- `launcher/`: launcher/update application, with a separate .NET build.

Inspect the working tree first and preserve unrelated work. Start from the files
implementing the behavior; do not rely on historical test counts or old plans as
proof of the current checkout. Avoid unrelated refactors and dependency upgrades.
Keep vendored notices and document intentional patches. A submodule change needs
a publicly available commit before updating the parent repository's pin.

Linux x86-64 development uses Nix and the patched engine:

```sh
./build.sh
./run.sh --desktop
./run.sh --editor
```

See [building](../docs/BUILDING.md) for prerequisites and other workflows. Stock Godot
is not a supported replacement. GDScript-only edits usually need a restart;
native changes need a rebuild. Use `--refresh-deps` when changing pinned native
dependencies, not as a reflex for every edit.

Run checks appropriate to the change and report the exact evidence. Typical
Rust checks are `nix develop --command cargo test --lib` and
`nix develop --command cargo clippy --all-targets -- -D warnings`; see
[testing](../docs/TESTING.md) and the relevant XR/feature test document for fixtures.
Isolate test preferences with `XDG_DATA_HOME`. Do not restart shared VR/audio
services, take over an active headset session, or change host configuration
without the operator's permission. Public-network tests and hardware tests have
different requirements from local fixtures.

A successful build is not a runtime test; Wine is not native Windows; submitted
frames are not proof of visible/comfortable headset output. Mark missing hardware
checks as untested and provide a precise handoff. Small documentation-only changes
need link/consistency checks, not a full engine rebuild.

No contribution should need the maintainer's home directory, private notes,
release-signing key, or production host. The present social target is one shared
friends-scale lobby; its intentionally shared configuration is not a private-room
security boundary. Use a fresh isolated lobby for automated tests so they do not
join real people. Do not copy private room values, signing credentials, personal
paths or raw support reports into a PR. Do not change official packages/feeds as
part of a normal contribution.

In the PR, explain the user-visible change, scope, design assumptions/discussion,
tests and their limits. The contributor must review the diff and understand the
behavior they submit. See [contributing](../CONTRIBUTING.md) for the short format.
