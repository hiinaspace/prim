# The room core (round R1 draft, 2026-11-11)

`prim-room` is the daemon that holds one room: membership, state, messages, and
streams later. Voice, the Godot room, the mpv shim and the tracker helper are local
clients of it. This file describes how the core (`crates/room-core`) behaves.
Wire formats are in [CONTRACTS.md](CONTRACTS.md).

The design comes from S's architecture reports (2026-11). Three things shape it:
- The room is three primitives over one membership: **state**, **messages** and
  **streams**.
- Rooms are ephemeral, for fewer than eight invited friends, like a living room.
- Authority is a small set of signed grants that converges by union. Everything
  else is single-writer.

## Layers

```
prim (multi-call binary): prim room | prim ctl
  prim-room (library): tokio driver, transports (iroh prim/1, in-memory), local API socket
    room-core (sans-IO): Room state machine
      grants (GrantSet)  staterec/state store  subscriptions  coordinator  frames  token
```

`room-core` never reads a clock, a socket or the OS RNG. The driver passes
`Now { mono_us, unix_ms }` with every input and drains outputs. That keeps the
deterministic simulator (`room_core::sim`) honest: it drives the same `Room` the
daemon does.

## Peer protocol

**Connecting.**
- When a connection comes up (`Input::Connected`), the dialler sends `hello` on
  lane 0 and the acceptor answers with its own `hello`. A side that hasn't
  received a hello within `hello_timeout` closes the connection.
- The acceptor admits the peer if one of these holds:
  - `GrantSet::admit(peer, now.unix_ms)` passes (an existing member, room open);
  - the hello carries a proof `[invite, member]` that passes `check_redemption`.
    The two records are then added and flooded.
- Otherwise it sends `reject` with a reason and closes. The dialler applies the
  same check to the acceptor, without the proof path: a joiner accepts any peer
  listed in its token, and learns the roster from the snapshot.
- `hello.room` must match, `hello.member` must equal the transport key, and
  `hello.version` must be 1.

**Duplicates.** When two live connections exist to the same peer, each side
keeps the one whose *dialler* key is lower and closes the other with
`duplicate`. Both sides compute this the same way, so they converge.

**After both hellos,** each side sends on lane 0:
1. `records`: every record in its grant set, genesis first.
2. `snapshot_begin`, one `state` frame per durable record it holds (all writers,
   presence included), then `snapshot_end`.
3. Deltas afterwards.

**Joining.**
- The joiner has no genesis until the first `records` frame arrives. It checks
  that the genesis hashes to the token's room ID, then builds its `GrantSet`.
- Until then it holds state records that arrive early (bounded) and ingests them
  once the set exists; the state lane can overtake the records lane.
- A sender ends its initial grant-set sync with `records_end` (0x14). The
  receiver buffers every `records` frame until then (bounded by the grant-set
  cap) and judges membership, removal and the open generation only on the whole
  set, so a grant or ancestor that rides in a later chunk can't read as "not a
  member". `records` frames after `records_end` are incremental floods and are
  applied one by one.

**Records** (grants, lifecycle, claims) are flooded:
- A node that inserts a new record sends it in a `records` frame to every
  other established peer.
- Duplicates are ignored, and so are records for another room.
- `removed` is derived from the grant set (and this node's own grant expiry) and
  recomputed whenever the set changes; a missing link that later arrives
  re-admits the node.
- Records that fail to decode are counted and dropped.

**Durable state** is flooded too, but only when the record improved this node's
state (a strictly greater version for that key, or a key it didn't have). Live
state is sent direct only and never forwarded.

**After any grant-set change,** every established connection is re-checked with
`admit`. Peers that are no longer members get `goodbye(revoked)` and are closed,
their state is dropped, and local clients get `Member::Removed`. If this node
itself is no longer a member, it closes every connection and emits
`LocalEvent::Removed`.

**Close.**
- A valid close makes every node flood it, emit `RoomClosed`, send
  `goodbye(closed)` to every peer and close.
- A closed node doesn't redial. It still accepts connections, so it can learn a
  reopen, but `admit` rejects with `closed` while the room is closed.
- A keyholder's `Reopen` issues `open(g+1)`, where `g` is the closed generation.
  It then dials members, and their hellos carry `open_generation`.
- **Known gap:** a closed node learns about a reopen only when a keyholder dials
  it. That is why the keyholder dials every member after a reopen.

**Dialing.**
- An open, established node dials every roster member it has no connection to.
  It dials immediately when it first learns of the member, then every `redial`
  interval while that member stays unconnected.
- Hints come from presence and from the token.
- The driver resolves a key to an address (iroh discovery, or the in-memory
  transport's registry).

## State store

**Keys.**
- Records are keyed by `(scope, writer, name, class)`; for room scope the
  writer is not part of the key.
- A record replaces the stored one only if its version is strictly greater.
- Room-scope records are valid only if `writer == holder_of(epoch)`.
  - When a claim changes the best holder of an epoch, re-check that epoch's
    stored records and drop the ones that no longer qualify.
  - Fall back to the next-best stored version. To keep the fallback simple, the
    store holds, per room key, the best valid record of **each epoch** that has
    one.

**Member-scope records** are accepted only from members (set validity).

**Visibility.**
- A presence-bound member key is visible only while all of these hold:
  - its writer's presence is online;
  - the presence is fresh, or within rejoin grace;
  - the record's `(generation, term)` equals that presence record's.
- A presence-bound room key is visible only while `lifecycle == Open(record.generation)`.
- Persistent keys are always visible.
- Pruning: presence-bound records of a writer that has stayed absent past grace
  are deleted. Visibility changes emit `Removed` or `Value` to the subscribers
  whose patterns match.

**Caps.** Enforce the CONTRACTS.md caps on local writes, with an `OverBudget` or
`TooLarge` reply, and on remote records, which are dropped and counted. Budget
usage appears in `Status`.

**Own writes.**
- Version is `(open generation, member_incarnation, ++seq)` for member scope,
  and `(coordinator epoch, ++seq)` for room scope.
- A room write fails with `NotCoordinator` unless this node holds the current
  claim.
- If this node receives its own member-scope record at a version ≥ its local one,
  it sets `seq` past it and re-publishes its current value. Presence uses the same
  rule. This is the Yjs re-assert.

**Presence.**
- On becoming established in an open room, this node writes
  `member/<me>/presence` (online, `since_ms` = when this node joined the current
  generation, `daemon_incarnation`). It re-writes it every
  `presence_interval`.
- On `Leave` it writes offline, floods it, sends `goodbye(leave)` and closes. Leaving
  isn't revocation: the member grant stays.
- Freshness is per writer: `last_arrival_mono` of a strictly newer presence version.
  - A writer is fresh while `now − last_arrival < presence_expiry`.
  - A writer is unresponsive past that.
  - A writer is offline on an empty presence value.
- `MemberEvent`s fire on each transition.
- QUIC disconnect is the fast path: a peer with no connection *and* no newer
  presence arriving via others is marked unresponsive at
  `min(expiry, disconnect + presence_interval)`.

## Subscriptions

**Patterns** are `/`-separated segments. `*` matches exactly one segment, and a
trailing `**` matches one or more. Examples: `member/*/pose`, `room/**`, `**`.

**Lifecycle.**
- On subscribe, the client gets `Reset`, then the current visible matches as
  `Value`, then `SnapshotDone`.
- Each change to a visible matching key marks it pending.
- `drain` returns pending entries, coalesced to the latest value per key.
- A `Ready` event is emitted once each time the pending set goes from empty to
  non-empty, so a slow subscriber holds one value per key and never builds a
  backlog.

**Order.** Within one key, a subscriber never sees an older version after a newer
one.

**Reset after reconnect.** When the node re-establishes after having no peers,
every subscription gets `Reset`, then the snapshot.

## Coordinator

- The current coordinator is `GrantSet::coordinator()`, of the open generation.
- Liveness comes from the holder's presence:
  - offline value ⇒ `Left`;
  - no newer presence for `coordinator_liveness` ⇒ `Unresponsive`.
- In either case, every node computes the **candidate**. The candidate is the
  present, fresh member with the lowest `(since_ms, key)`, excluding the current
  holder. If this node is the candidate, it issues
  `claim(g, current_term + 1, rank = own since_ms)`.
- A node that sees a higher claim emits `Changed`, including the old coordinator
  when it returns. Nobody re-claims while the current holder is fresh.
- **Partition.** Both sides may claim the same term. When they merge, the lower
  `(rank, key)` wins. The loser's room writes at that epoch become invalid and
  fall back.
- **Coordinator messages.** A `Send` to `To::Coordinator` resolves the holder at
  send time. It fails with `NoCoordinator` when there's none or the holder is
  unresponsive, and goes directly to the holder when the holder is this node's
  own local clients.

## Messages and datagrams

**Send.**
- `Send` resolves its targets at send time:
  - `Member(k)` must be connected, else `Offline`.
  - `All` means every connected member.
  - `Coordinator` is described above.
- The reply is `Sending { msg_id, targets }`, followed by one `MessageResult` per
  target. The result is the ack, `Lost` if the connection drops first, or
  `Offline`.
- Unreliable sends get no ack. They are reported `Acked(Delivered)` at hand-off.
- Nothing is queued.

**Receive.**
- If a local client registered the channel, deliver to it and ack `delivered`.
- Otherwise drop and ack `no_channel`. A non-member sender gets `not_member`.
- A payload over 64 KiB gets `too_large`.

**Datagrams.** App datagrams go to registered channels only. The
receive age is `arrival_ms − send_ms` minus the per-peer minimum of that
delta. The sender passes `max_age` through to the driver.

## Simulator (`room_core::sim`)

- A seeded N-node scheduler drives `Room`s through an in-memory network. Each
  pair of nodes has one ordered, reliable link per lane, plus unordered lossy
  datagrams.
- The seed controls:
  - per-message latency (base plus jitter);
  - cross-link reordering;
  - datagram loss and reorder;
  - partitions (sets of cut links over time);
  - node stops and restarts. A restart is `resume` from the last persist, with
    `member_incarnation + 1`.
- Time is virtual. The scheduler advances to the earliest of the next delivery
  and the nodes' `poll_timeout`.
- Oracles assert convergence: equal grant sets, equal visible durable state,
  and one coordinator once the network is healed and quiet.
