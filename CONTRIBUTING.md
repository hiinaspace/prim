# Contributing to Prim

Human and agent-assisted contributions are welcome. Useful work leaves another
person able to understand, build, test, and maintain the change. You do not need
the maintainer's private notes or development environment; missing context should
be added to this repository.

Prim's aim is a [shared virtual living room](docs/DIRECTION.md). The maintainer
coordinates the architecture and product direction, while many improvements to
everyday use can be developed independently. Read [AGENTS.md](AGENTS.md) when
using an agent, and give it the specific task and any relevant design discussion.

## Useful contributions

These are useful starting areas, not claims that every item has a settled
implementation design or an active assignee. Keep a PR focused on one observable
improvement. A short issue helps avoid duplicate substantial work; routine fixes
can go directly to a PR.

| Area | Useful scope | Evidence and boundary |
| --- | --- | --- |
| Menu usability | Readable layout, focus/navigation, text entry, controller interaction, clearer existing controls | Show before/after views and exercise desktop and relevant VR inputs. Preserve microphone, deafen, local-media and shared-media semantics. Start in `project/ui/world_menu.gd`. |
| Nameplates | Legibility, placement, speaking indication, local presentation preferences | Check distance, occlusion, several avatars and different display names. Preserve the head/voice anchor; keep identity/moderation redesign separate. Start in `project/net/avatar.gd`. |
| Full-body tracking | End-to-end tracker sampling, calibration, avatar IK, remote replication and tracking-loss fallback | Necessary protocol changes are welcome. Keep head/hands-only and desktop working. State hardware tested, coordinate frames, recenter behavior and tracker loss; document the wire change and test matching peers. Start with `project/avatars/`, `project/net/pose.gd` and [avatar boundaries](docs/AVATARS.md). |
| Runtime/platform compatibility | Reproduce and fix overlay, controller-binding, runtime discovery or packaging failures on another supported target | Record OS, GPU, runtime, controllers and exact revision. Test visible output, input and scene/game handoff on actual hardware where relevant. Identify any new process/backend assumptions. Start with `project/xr/` and [OpenVR qualification](build-support/openvr-overlay/README.md). |
| Build and diagnostics | Reproduce a clean build, remove accidental local dependencies, improve actionable errors and focused regression coverage | Explain the failure and verify the documented path. Keep credentials and private support reports out of the PR. Start with [building](docs/BUILDING.md). |
| Existing behavior bugs | A small fix in any subsystem with a clear reproduction | Preserve the existing design and exercise the failure plus a relevant neighboring case. Networking/media directories are not off limits. |

FBT is welcome as a working end-to-end prototype, even if its wire representation
needs revision before merging. Local calibration/IK work can also be useful on
its own. Likewise, qualifying the existing SteamVR peek path on Windows is useful
without proposing a resident service. Clearly label experiments and partial coverage.

## Architecture experiments and maintainer direction

UGC avatar/world loading or distribution, persistent editable worlds/CRDTs,
identity/lobby/discovery or moderation changes, new streaming architecture, and
session/process/SDK ownership need particular attention to the intended direction.
These areas interact with long-term plans; a locally attractive shortcut can make
the intended design harder to build. The [direction document](docs/DIRECTION.md)
explains the constraints and links the deeper plans.

Exploratory draft PRs are welcome before agreement. A small working prototype can
be more useful than an abstract proposal, even when some of it will be replaced.
Do not present a prototype's shortcuts as settled upstream architecture. Make its
assumptions and departures from the plans easy to find. For a larger commitment,
an early discussion helps avoid spending effort on the wrong direction.

An issue, draft PR, or short design note can say:

1. What user behavior improves, and what the current code does.
2. Which contract changes, and how the proposal fits the linked direction.
3. The smallest implementation slice, compatibility/failure behavior, and tests.
4. What remains open or deliberately deferred.

Link any relevant maintainer discussion. Routine implementation decisions do not
need repeated approval. If a focused feature starts requiring a foundational
rewrite, split out the experiment and explain why. The maintainer retains the
high-level architecture and merge/release decisions; neither an old plan nor a
contributed prototype makes a new direction binding. Preserve useful separable
pieces so a partially adopted experiment still helps the project.

## Releases, compatibility and the shared lobby

The official user-facing build is signed and published by the maintainer at
[prim.hiina.space](https://prim.hiina.space/). Test branches may require all peers
to run the same candidate. We do not currently promise compatibility across
arbitrary versions or independently maintained clients. A necessary wire change
should update its version/schema, validation, fixtures and [protocol docs](docs/PROTOCOL.md)
as appropriate; explain how mismatched peers fail instead of inventing a broad
negotiation framework solely to keep old branches working.

The intended initial public testing arrangement is one shared lobby at
friends-only scale. Its shared configuration may be distributed to participants;
that is not a claim of private membership or production moderation. Private-lobby
UX and a different identity/lobby structure may follow soon. Do not treat this
temporary arrangement as a requirement to preserve in every future design.

## Build and test your change

Start with [building](docs/BUILDING.md). The shortest maintained source-development
path is Linux x86-64 with Nix, `./build.sh`, then `./run.sh --desktop`. The project
requires its patched Godot build. Windows cross-build and launcher instructions
are separate; a native Windows one-command source build is not currently documented.

Choose checks for the behavior changed; [testing](docs/TESTING.md) includes local
fixtures, multi-process integration tests, historical evidence and manual gates.
Read dated evidence as a record of that revision, not a current support guarantee.
Keep automated/headless, rendering, actual-headset, WAN, and subjective checks
distinct. If you lack a device, provide a reproduction/test procedure and say
what still needs a tester. Do not report it as passing.

Source builds generate their own ignored `project/private_lobby.json`. To test
with friends, securely share a development room configuration among those builds.
This creates a separate room, not automatic access to the official shared lobby.
Automated tests should use fresh isolated configurations, not real participants'
room. Do not include private room values or signing keys in reports. Public
distribution of the intended common lobby configuration is a release/onboarding
choice, not a reason to copy arbitrary local secrets into source control.

## Submitting agent-assisted work

The contributor remains responsible for the result: review the diff, understand
the changed behavior, and be able to explain the evidence. State how the change
was validated and what still needs human/device testing. Model names, generated
reasoning transcripts, and long activity logs are not a substitute for that.
Keep generated reports small and free of personal information.

A useful PR body is:

```text
Problem and resulting behavior:
Scope, design assumptions and linked discussion (especially for experiments):
Validation (commands, platform/hardware, observations):
Untested cases / follow-up needed:
```

Include screenshots or a short clip when visual behavior matters, and a focused
regression test when it usefully captures the failure. Avoid broad formatting
changes, speculative frameworks, unrelated upgrades, and tests that only repeat
the implementation. Explain any new dependency or protocol change.

Keep third-party code, models and their notices distinct from original Prim code.
Original contributions use the [Unlicense](UNLICENSE); modifications to third-party
or derived material retain its applicable terms. See [licensing and attribution](LICENSES.md).
