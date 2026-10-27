# Identity round 1: persistent keys, friends, Friends/Invite instances

Status: plan for branch `claude/identity-round1` (stacked on
`claude/role-bound-admission`). Owner decision 2026-10-27: the first identity
round is keys + friends + Friends/Invite instances; take ideas from atproto
without adopting it; trust ranks are parked.

## Target

Prim's MVP is the maintainer's own VRChat use: friends-only instances of about
ten people who know each other, mostly watching videos and talking. Match
VRChat's UX at that scale with no infrastructure beyond what Prim already uses
(static hosting of signed builds, iroh's public relays/address lookup, mainline
DHT for the shared lobby).

**Property the owner judges (headset):** after friending once, my friend and I
restart Prim; I host a Friends instance, my friend presses **Join** next to my
name in their Friends list and we are in the same room with voice and video,
while someone not on my list who tries the same is refused.

## Design

### Identity

- Each install generates one Ed25519 key on first run and stores it at
  `user://identity.key` (32 raw bytes). It is the iroh endpoint secret key, so
  the endpoint ID *is* the person's Prim ID. iroh's TLS already authenticates
  that key on every connection; admission needs no extra proof for identity.
- Shown to the user as a **friend code** (the endpoint ID string) in the
  Friends tab, with Copy.
- Display names stay self-claimed. Friends are shown by **petname**: the name I
  saw (or typed) when I added them, stored locally. A peer renaming themselves
  cannot impersonate one of my friends in my lists (atproto/SSB petname idea).
- Deliberate departure from the research recommendation (root → device →
  session certificates): round 1 uses one key per install. Costs: two machines
  are two identities, the key is linkable on relays/DHT, and losing the file
  loses the identity. Migration path: round 2 makes this key the first device
  key under a new root key and friend records move to the root ID; existing
  friendships carry over because the old key signs the root binding.

### Friends

- `user://friends.json`: `[{id, name, added}]`, where `name` is my petname.
- Two ways to add:
  1. **In a room** (VRChat-style): People list → **Add friend** on a peer sends
     control message `{"type":"friend_request","name":…}` to that peer. The
     receiver sees **Accept**. Sending a request to someone who already asked
     you, or accepting, makes both sides store each other. The sender ID comes
     from the authenticated connection, never from the payload.
  2. **By code**: paste a friend code + optional name → **Add**. One-sided:
     it means "I admit this key". Used to bootstrap without a shared lobby
     (send codes over Discord).
- Remove deletes locally. No graph is published anywhere.

### Instances

| Mode | Who may join | Discovery |
|---|---|---|
| Shared lobby (existing) | anyone holding the build's lobby secret | DHT record from the secret |
| **Friends** | the host's friends list | friends dial the host's ID |
| **Invite** | IDs the host ticked as invited (from their friends list) | invitees dial the host's ID |

- Hosting a Friends/Invite instance starts the room with a fresh random
  32-byte **instance secret** and no DHT publication. The host's endpoint also
  accepts ALPN `prim/join/1`.
- Joining: the joiner dials the host's ID with `prim/join/1`. The host checks
  `connection.remote_id()` against its current admission set. If admitted, it
  replies `{version, secret, host: Address, mode, name}`; otherwise it closes
  with a "not admitted" reason (or "not hosting"). The joiner then runs the
  existing room protocol with that secret and the host as direct host, so the
  mesh, role-bound proofs, media and file sharing are unchanged.
- Defense in depth: while hosting with a policy, the host also rejects room
  (`prim/friends/1`) connections whose remote ID is not admitted.
- The admission set is live: adding a friend or ticking an invite mid-session
  takes effect for the next join without restarting the room.
- Hello version goes to 6 (identity persistence and the new ALPN; peers update
  together).
- Known limits, accepted for round 1: the instance secret is shared with
  members, so a member removed mid-session could still re-join that instance
  until the host starts a new one; members admit each other on the host's word;
  the host leaving ends the instance (existing behaviour).

### What round 1 does not do (next steps, in order)

1. **Presence**: see which friends are online and in which instance. Needs an
   endpoint that lives for the whole Prim process (today it lives only for a
   room), then friend-gated status replies on `prim/join/1` (or a presence ALPN).
2. **Invite push / request invite**: send "X invited you" to a friend's running
   Prim. Same dependency as presence.
3. **Friends+** (friends of anyone in the instance) and Invite+.
4. Root/device keys and recovery (research recommendation 1), block list.
5. Trust ranks: parked by owner decision.

## Build steps (this round)

1. Native: optional persistent secret key in `Handle::start`; `Mode` =
   shared lobby | host(policy, admission set) | join(host address); `prim/join/1`
   accept/dial; host-side admission check in `establish`; `set_admission(ids)`;
   expose identity generation/ID helpers to GDScript. VERSION 6.
2. GDScript: load/create identity; friends store; Friends tab (code + Copy,
   paste-to-add, list with Join / Invite toggle / Remove); host buttons;
   in-room Add friend / Accept; join by friend.
3. Tests: native unit/integration (below); a GDScript test for the friend
   request flow and persistence; a two-process headless run where B joins A's
   Friends instance by ID and a third process with an unknown key is refused.
4. Docs: PROTOCOL.md (join ALPN, version 6), TESTING.md manual steps.

## Headless evidence gate

- Same key file → same endpoint ID across two starts.
- Friends host admits a listed key, the joiner reaches Connected and full mesh
  with a second admitted joiner; an unlisted key is refused and never appears
  in `peers` on either side.
- Invite host refuses a friend until ticked, then admits after `set_admission`.
- Friend request: mutual add persists to `friends.json` on both sides; a spoofed
  `id` field in the payload is ignored.

Not provable headlessly: WAN dialing by ID through iroh relays between two
homes, menu usability in the headset, and whether it feels like VRChat. Those
are the owner's gate.

## Implementation record (2026-10-27)

Implemented as designed, with these notes:

- `Handle::start_with(Options)` carries key, mode and local-only; `Handle::start`
  remains for the shared-lobby tests. Rust `Mode` = Shared | Host | Join.
- Join refusals use application close codes 3 (`not admitted`), 5 (`not hosting`)
  and surface as `Join: <reason>`. The in-room defense (unadmitted
  `prim/friends/1` dialers) is checked before the hello exchange.
- Invite ticks are held in memory per hosted instance (not in `friends.json`).
- The Invite toggle is only shown while hosting an Invite instance.
- A network error that ends a session now stays in the status line after the
  session closes (otherwise "not admitted" was overwritten by "disconnected").
- Joining by bare ID relies on iroh's N0 preset (PkarrPublisher + PkarrResolver +
  DNS address lookup are configured by `presets::N0`; confirmed in the iroh 1.x
  source). Not exercised over the real internet. Local tests use `Minimal` plus a
  full Address, passed to the app via `PRIM_TEST_HOST`.
- The endpoint ID is now stable in the shared lobby too (same key), so it is
  linkable across DHT records and relays, as the identity note already accepts.

Evidence: `cargo test --lib network::` (Friends admits two listed keys to a full
mesh; unlisted key refused at join and with a leaked secret; Invite refused then
admitted after `set_admission`; same key gives same ID; non-hosting refuses),
`tests/friends.gd`, `tests/identity.gd`, and `tools/test-friend-join.py`
(three headless processes: host, listed friend presses Join, unlisted intruder
refused; pose and synthetic voice decode both ways). Not run: WAN by-ID dial,
headset/laser use, real microphones.
