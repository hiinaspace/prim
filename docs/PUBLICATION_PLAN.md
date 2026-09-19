# Source publication preparation

Preparation and validation record, 2026-09-19. The maintainer approved the source
license, dependency publication, intentional snapshot and GitHub visibility change.
Source publication
and opening contributions are separate from distributing new binaries. The
maintainer accepts an initially shared public lobby at friends-only scale.

## Proposed sequence

1. Review [direction](DIRECTION.md), [agent guidance](../AGENTS.md) and
   [contribution scope](../CONTRIBUTING.md). Finalize the
   [license treatment](LICENSING_PLAN.md). Keep useful design rationale here.
2. Prepare an intentional source snapshot, preserving unrelated work in progress.
   Include the currently untracked design docs referenced by the guides. Normalize
   public-facing instructions and finish the targeted source/history review.
3. Validate anonymous dependency access and a fresh source build without the
   maintainer's staged outputs, personal configuration, private room or notes.
   First publish the missing libmpv-zero dependency commit identified below, after
   reviewing its contents. Have a fresh-context agent attempt the documented build
   workflow. Record failures and fix the instructions/code as needed.
4. Review the exact publication commit, license/notices, remaining platform limits
   and contribution docs. Then explicitly choose to make the repository public.
   Do not rewrite history for harmless personal paths; address actual sensitive
   material if the targeted review finds it.
5. After publication, verify anonymous recursive clone and linked documentation.
   Seed a small set of scoped contribution issues, with acceptance criteria and
   hardware needs. Add a minimal PR template and optional CI once the workflow
   and checks have been agreed and reproduced.

## Inspection so far

- The root has no `LICENSE`; `Cargo.toml` declares MIT for the native crate. The
  maintainer prefers Unlicense/WTFPL; the recommendation is Unlicense for original
  code with explicit exceptions. See [licensing findings](LICENSING_PLAN.md). Keep
  third-party code, models, engine patches and runtime notices under their own
  terms. [Avatar provenance](../project/avatars/models/ATTRIBUTION.md) already
  records different asset terms and a Vita source-notice/metadata discrepancy;
  resolve the distribution basis before making blanket licensing claims.
- **Missing public dependency commit:** Prim's committed libmpv-zero gitlink is
  `293d895d0e11eca1b3665a654fe4438e2d9ea031` (subtitle/live controls). Anonymous
  GitHub commit lookup returns "No commit found"; public `main`/HEAD still points
  to `11c6e22bd07496bb278ed2d2836de9370e73d2d8`. An anonymous fetch in a new bare
  repository also failed with `not our ref`. Publishing Prim alone cannot make
  that dependency available. Review/publish the intended dependency commit before
  the fresh recursive-clone gate; do not silently downgrade its pin.
- Exact pins for godot-network-audio, godot-cpp, the patched
  videocall-rs, mpv and the separately fetched OpenVR overlay are anonymously
  accessible through GitHub's commit API. This is not a complete recursive fetch
  or downloaded-artifact/build check.
- `build.sh` uses repository-relative paths, initializes missing submodules,
  enters Nix, stages its patched engine/extensions and creates a new ignored lobby
  secret. This is promising, but an existing successful developer tree is not
  evidence of a fresh build. Windows remains a multi-stage cross-build recipe.
- A narrow scan of tracked text found no private-key blocks or GitHub/AWS token
  patterns. No tracked private-lobby/key/env filenames appeared in the selected
  filename-history check. This was not a comprehensive history/content scan.
- Existing unrelated runtime/avatar changes and several untracked plans are in
  the working tree. Do not publish by blindly staging the whole directory.
- A fresh-context subagent, restricted to repository documents/code, correctly
  interpreted the living-room direction, direct fixes, end-to-end FBT, matching
  candidate clients and exploratory architecture PRs. It found the policy inviting.
  Its stale-link/status and packaged-Windows test-room findings were corrected.
  This was a reading exercise, not a fresh build or hardware qualification.

## Documentation cleanup inventory

| Finding | Public-source treatment |
| --- | --- |
| Private tracker links in `AVATAR_PLAN.md` and `EXPERIENCE_CONTINUITY.md` | Entry-point rationale has been inlined and private tracker requirements removed in this drafting pass. Keep historical credit/provenance where useful. |
| Local source paths in avatar, viseme, XR and media research plans | Label them as historical inspection context, replace actionable links with a public pinned reference or an in-repo explanation. Never require access to those checkouts. |
| Live test URLs in `LIVESTREAM_TEST.md` | The normal recipe now uses a reader-controlled MediaMTX endpoint. The maintainer's test server is not shared contributor infrastructure. |
| Host and signing-key paths in `launcher/hosting/DEPLOYMENT.md` | Separate a reusable local packaging recipe from historical production operations; parameterize actionable examples. A private key path is not the key, so it alone is not a history-rewrite reason. |
| Old "current" release/protocol claims in `TESTING.md`, `AVATARS.md`, `LAUNCHER_PLAN.md` | Distinguish current instructions from dated evidence; use `PROTOCOL.md` as the wire-reference entry point and identify the exact tested release when preserving reports. |
| Design documents currently untracked | Include selected docs in the publication commit and verify all relative links in that commit, not only on disk. |
| No simple contributor entry point | The new direction/agent/contribution drafts provide purpose, scope examples, code entry points and evidence expectations. |

The first documentation pass should fix broken requirements and misleading
instructions. It need not erase every developer name, local path or historical
experiment. Avoid copying raw private notes/support reports into the repository.

## Clean-build and publication evidence to collect

- A selected commit can be cloned recursively without owner credentials; every
  submodule, Git dependency and pinned source revision is accessible anonymously.
- On Linux x86-64 with Nix configured for flakes, the documented build and desktop
  launch work in an isolated checkout without borrowed `.local`, `target`, imported
  assets, or a maintainer lobby. Record revision, environment, time/disk needs and
  failures. No root/production credentials should be needed after prerequisites.
- A second isolated build/configuration can use an explicitly shared test secret
  for a room smoke test; unrelated friends cannot be joined accidentally.
- Relevant Rust and focused script checks pass. Actual native Windows, headset,
  WAN/relay and runtime/controller coverage stay explicitly limited to tested cases.
- Review tracked assets/notices and the selected history for real credentials,
  embedded private room values and restricted material. Report candidate paths
  privately without printing secret values. Preserve history unless findings
  require a specific remedy; rotate/revoke any exposed credential if applicable.
- Documentation links resolve from the selected commit, including a clear build
  route, purpose, contribution boundaries, and the source license.

## Agreed contribution policy

- Exploratory draft PRs are welcome before architecture agreement. Explain
  conflicts/assumptions; the maintainer decides adoption, merging and releases.
- End-to-end FBT, including necessary wire changes, is welcome. A working
  prototype remains useful even if its protocol is later revised.
- The maintainer-signed build at prim.hiina.space is the official user-facing
  release. Test branches can require matching clients; broad version/fork
  compatibility is not a current requirement.
- One shared lobby at friends-only scale is acceptable initially, with private
  rooms/lobby UX likely to evolve. Automated tests still use isolated rooms.
- Prefer Unlicense/WTFPL for original code; accept required dependency copyleft.

Remaining review: exact license/third-party treatment, intended publication
snapshot, source-build evidence, and how source users obtain the intentionally
shared lobby configuration if they want it. Keep the default generated development
room isolated. Seed one UI/nameplate issue, an end-to-end FBT prototype, and one
specific runtime qualification issue when opening contributions.

Source commits and the GitHub visibility change are authorized. Binary release
publication and production configuration changes are outside this source pass.
No history rewrite is planned.
