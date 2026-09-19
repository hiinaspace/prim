# Working on Prim

Prim is a shared virtual living room: voice, avatars and shared viewing should
stay useful as friends move between activities.

- Read [CONTRIBUTING.md](CONTRIBUTING.md) for contribution scope and PR expectations.
  UI/nameplates, end-to-end FBT (including necessary wire changes), focused fixes
  and platform compatibility work are welcome without advance design approval.
- For UGC/world loading, CRDT/persistence, identity/P2P/lobbies, streaming, or
  process/SDK ownership, read [DIRECTION.md](docs/DIRECTION.md) and its relevant
  linked plan. A locally attractive shortcut can make the intended design harder
  to build. Exploratory draft PRs are welcome: explain departures, keep them
  reversible, and do not silently make them upstream architectural commitments.
- The maintainer decides adoption and publishes the official signed build.
  Test branches may require matching peers; broad version/fork compatibility is
  not a current requirement.
- Inspect the working tree and preserve unrelated work. Keep changes focused;
  read dependency instructions before editing submodules or vendored code.
- See [BUILDING.md](docs/BUILDING.md) for `./build.sh` and `./run.sh --desktop`.
  Use the patched engine. Native edits need a rebuild; GDScript usually needs a restart.
- Read [DEVELOPMENT.md](docs/DEVELOPMENT.md) for contracts and code entry points,
  and [TESTING.md](docs/TESTING.md) for appropriate checks. Report actual evidence
  and limits: builds, Wine and headless tests do not prove native headset behavior.
- Isolate test rooms/preferences. Do not disrupt shared VR/audio services or an
  active headset session without the operator's permission. Never commit private
  credentials, personal support reports or production signing keys.
