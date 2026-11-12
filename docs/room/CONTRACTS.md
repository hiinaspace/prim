# Room wire contracts (rounds R1 and R0, 2026-11-11)

These are the wire contracts for the `prim-room` daemon. The architecture is in
[ROOM.md](ROOM.md), and voice in [VOICE.md](VOICE.md). Contracts 1–3, 7 and 8 are
drafts to freeze before friends get a build. Contracts 4 (hello), 5 (datagram
header) and 6 (voice envelope) were frozen in round R0.

Golden vectors for every encoding are in
`crates/room-core/tests/vectors/golden.json`. `cargo test -p room-core
--test golden` checks them; regenerate with `PRIM_UPDATE_VECTORS=1` only for a
deliberate wire break.

## 0. Conventions

- Integers are little-endian and fixed width. There are no varints, so every
  value has exactly one encoding.
- `bytes16` is a `u16` length followed by that many bytes. `bytes8` uses a `u8`.
- Keys are Ed25519 public keys (32 bytes). A member key is the daemon's iroh
  `EndpointId`, so the QUIC handshake authenticates it.
- Signatures are Ed25519 and are verified with `verify_strict` (rejects small-order
  keys and non-canonical `S`).
- `H(ctx, bytes)` is BLAKE3 `derive_key(ctx, bytes)`, 32 bytes.
- Decoders are strict: exact lengths, no trailing bytes, reserved fields and
  bits must be zero, unknown kinds are rejected. A record that fails to decode is
  dropped and counted; it is never forwarded.

## 1. Room ID

A room is founded by a **genesis record** (record kind 0, below) signed by the
room's **root key**. Its body is `nonce[16] || created_ms u64 || meta bytes16`.

```
room_id = H("prim/1 room-id", root_key[32] || nonce[16] || created_ms u64 || meta bytes16)
```

The root key sits outside the mutable grant set: it can't be revoked, retired or
out-ranked, and the room ID commits to it. Two rooms with the same root differ
by nonce. A peer that receives a genesis checks that it hashes to the room ID it
asked for.

## 2. Signed records (grants and room lifecycle)

Every authority fact is a signed record. They share one envelope:

| offset | size | field |
|---|---|---|
| 0 | 2 | magic `"PR"` |
| 2 | 1 | version = 1 |
| 3 | 1 | kind |
| 4 | 32 | room_id (all zero for genesis only) |
| 36 | 32 | issuer key |
| 68 | 32 | issuer_cites: a revocation of the issuer it signs under (all zero = none; must be zero for genesis) |
| 100 | … | body, per kind |
| end−64 | 64 | signature |

- `signature = Ed25519(issuer, "prim/1 record\0" || bytes[0 .. end−64])`
- `record_id = H("prim/1 record-id", bytes[0 .. end−64])`. The signature is outside
  the ID, so a re-signed copy can't masquerade as a different record.

### Kinds and bodies

| kind | name | body |
|---|---|---|
| 0 | genesis | `nonce[16] created_ms u64 meta bytes16` |
| 1 | keyholder | grant body |
| 2 | inviter | grant body |
| 3 | member | grant body |
| 4 | invite | grant body (`subject` is a one-shot ephemeral key) |
| 5 | revoke | `subject[32] nonce[16] issued_ms u64 cites_revoke[32]` |
| 6 | retire | `subject[32] nonce[16] issued_ms u64 cites_revoke[32] kept_count u8 kept[32 × n]` |
| 7 | open | `generation u32 nonce[16] issued_ms u64` |
| 8 | close | `generation u32 issued_ms u64` |
| 9 | claim | `generation u32 term u32 rank u64 issued_ms u64` |

Grant body: `subject[32] nonce[16] issued_ms u64 expiry_ms u64 generation u32
cites_revoke[32] meta bytes16`.

Field rules (decoder-enforced):
- `meta` is at most 256 bytes. It is a petname hint, never a store.
- `generation` is nonzero for `invite` (the open generation the invite is bound
  to) and zero for every other grant.
- `expiry_ms` 0 means no expiry. Keyholder and inviter grants must have `expiry_ms = 0`
  (nothing enforces their expiry, so the decoder refuses to pretend).
- `cites_revoke` all-zero means none.
- `kept` holds at most 64 record IDs, strictly ascending.
- `issuer_cites` of genesis is zero.
- `generation` of open, close and claim is at least 1. A claim's `term` is at least 1
  (term 0 is the implicit claim made by the open).

### Authority

The grant set is a grow-only set of records. Validity is a **pure function of
the set**: no clocks and no arrival order, so every replica that holds the same
records computes the same roster. Expiry and the invite's generation binding are
admission checks (section 7), not set-validity rules.

A grant set holds at most 4096 records besides genesis. A peer refuses records
past the cap; a room of friends uses tens.

Let `root` be the genesis issuer. Evaluate in this order:

1. **Root and self revocations always take effect.** A revoke or retire takes
   effect when it is issued by root, or by its own subject (leaving, or giving
   up authority). Revokes or retires that target root are ignored.
2. **Keyholders** are the subjects of keyholder grants issued by root that no
   effective revocation masks. Root counts as a keyholder.
3. **Keyholder revocations.** A revoke or retire issued by a keyholder takes
   effect unless its subject is root or a keyholder, or a stage-1 revocation
   masks it. A keyholder can't remove another keyholder; only root can. A
   revocation **kept** (below) by every stage-1 retire of its former-keyholder
   issuer also takes effect, so retiring a keyholder doesn't lift the bans it kept.
4. **Inviters** are the subjects of unmasked inviter grants issued by a keyholder.
   An inviter mints invites, never inviters.
5. **Invites** are unmasked invite grants issued by a keyholder or an inviter.
6. **Members** are the subjects of unmasked member grants whose issuer is a keyholder
   or the ephemeral key of a valid invite. An invite key may issue **one** member
   grant. If two distinct member grants share an invite key, both are
   invalid (equivocation: a leaked token can deny a join, never add a second
   member silently). Keyholders are members.
7. **Lifecycle.** Open and close records count when their issuer is a keyholder
   (or kept) and they extend the chain: see Room lifecycle. Claim records count when
   the issuer is a member and they extend the term chain: see Coordinator.

A record whose issuer is kept (below) satisfies "issuer is a keyholder" or "issuer
is an inviter" in stages 4–7.

**Masking.** An effective revocation `X` with subject `K` masks record `G` on two
sides, each with its own waiver. `G` is masked if either side applies unwaived.
- **Subject side.** A revoke reaches every grant whose subject is `K`; a retire
  reaches keyholder and inviter grants whose subject is `K` (`K`'s own member grant
  isn't touched). Waived when `G.cites_revoke` **covers** `X`: this is how a grant
  re-admits. Member grants issued by an invite key never get this waiver, because
  whoever holds the token chooses that cite; re-admitting a revoked person takes a
  member grant from a keyholder.
- **Issuer side (the cascade).** Both kinds reach every record whose issuer is `K`.
  Invites issued by `K` die, so the members those invites admitted die too.
  Waived when `G.issuer_cites` covers `X` (`K` signed `G` after it saw `X`), or, for
  a retire, when `G` is in that retire's `kept`. The waiver never grants authority:
  `G` still needs an issuer that holds it now (a keyholder, inviter, member…) by the
  stages above, or is kept.
- **Kept.** `G` issued by `K` is kept when at least one effective retire of `K`
  exists, every effective retire of `K` lists `G` in `kept` (`issuer_cites` does
  not substitute here: a record signed after a retire can't be in that retire's
  list), and `K` really held the authority: a root-issued keyholder grant for `K`
  (or, for invites, a keyholder-issued inviter grant) that no *revoke* masks. A
  member with no authority can't self-retire to launder grants it signed; a
  retiree can't sign more after its first retire and keep them with a second;
  and a later revoke of `K` still cascades past the kept list.

Nodes sign every record with `issuer_cites` set to the latest effective revocation
of their own key that they know, so a re-admitted or retired-but-still-member key
can still claim, invite or open.

`covers(c, X)` holds when `c == X.id`, or when `c` names a revoke or retire `C` in the set
with `covers(C.cites_revoke, X)`. A revocation's own `cites_revoke` names
the latest effective revocation of the same subject that its issuer knew. So
re-admission is a **new grant citing the revocation**, never an undo. A revocation
the re-admitter hadn't seen still wins, because tombstones beat reintroduction. A
missing link in the chain also means "masked", which fails safe and heals once
the record arrives. A cite is one chain, so two concurrent revocations of the same
key that neither cites can't both be covered: they are final in v1 (fail-safe).
A node refuses locally to issue a grant that wouldn't take effect.

Revocations issued by `K` are records issued by `K`, so revoking a hostile
keyholder also voids the revocations it made.

### Room lifecycle

Generations step by one. `open(1)` counts; `close(g)` counts only when an
`open(g)` counts; `open(g+1)` counts only when a `close(g)` counts. Records past
the chain don't count (they wait, harmlessly, in the set). So:
- `open_generation` is the last generation in the chain with a valid open.
- The room is **closed** if a valid close of `open_generation` exists. Close
  beats open at equal generation, so replaying an old open never reopens a room.
- Reopening issues `open(g+1)` after `close(g)`.
- The room is **dead** if it has never been opened.
- Nobody can jump a counter to its maximum: a `close(u32::MAX)` is inert.

### Coordinator

- Each generation `g` has an implicit claim `(g, term 0)`, held by the issuer of
  the valid open for `g` with the lowest record ID.
- Explicit claims carry `term ≥ 1` and `rank` (the claimant's presence `since_ms`).
  Terms step by one: a claim at `(g, t)` counts only when a claim at `(g, t−1)`
  counts (term 0 is the open). A node probes a claim before issuing it and never
  claims a term twice; a term that would overflow isn't claimed.
- The **best claim of a term** is the one with the lowest `(rank, holder key)`.
- The **current coordinator** is the best claim of the highest term.
- Apps key on `(generation, term, holder)`, never on "who is coordinator".

## 3. State records

State is keyed and single-writer. `member/<writer>/<name>` is written only by
that member. `room/<name>` is written only by the holder of the best claim
for the record's `(generation, term)`. Scope byte 2 (`obj/`) is reserved and
rejected.

| offset | size | field |
|---|---|---|
| 0 | 2 | magic `"PS"` |
| 2 | 1 | version = 1 |
| 3 | 1 | flags: bit0 `live` (else durable), bit1 `persistent` (else presence-bound); others zero |
| 4 | 1 | scope: 0 member, 1 room, 2 reserved (`obj`) |
| 5 | 32 | writer key |
| 37 | 4 | generation |
| 41 | 4 | term |
| 45 | 8 | seq |
| 53 | 1 | name length, 1..=48 |
| 54 | n | name: `[a-z0-9._-]` segments joined by `/`, no empty segment |
| … | 2 | value length |
| … | v | value (empty = deleted) |
| … | 64 | signature, **durable only** |

- **Signature.** `signature = Ed25519(writer, "prim/1 state\0" || room_id || bytes[0 .. end−64])`.
  The room ID is in the signature, not the bytes, so a record can't be replayed
  into another room.
- **Live records** are unsigned. They are accepted only from the writer's own
  connection, and the hello binds that connection to the room. They are never
  forwarded.
- **Epoch.** For member scope, `(generation, term)` is the open generation plus the
  writer's **member incarnation**, a counter the daemon persists and bumps on
  every start. For room scope it is the coordinator epoch.
- **Ordering.** Records order by `(generation, term, seq)` **per (writer, scope, name, class)**.
  There is no ordering across keys, classes or lanes. A reader keeps the greatest
  version and drops anything not greater.
- **Caps.**
  - Live values: ≤ 200 B.
  - Durable values: ≤ 4096 B. Larger blobs are referenced by hash.
  - Per member: ≤ 128 durable keys and ≤ 64 KiB of durable values.
  - Room scope: ≤ 128 keys and ≤ 64 KiB.
  - Over-cap writes fail locally with a reason, and over-cap remote records are dropped.
- **Lifetime.**
  - A presence-bound member key is visible only while its writer is present and
    its `(generation, term)` equals the writer's current presence record. A
    restart (new incarnation) or a new generation retires it.
  - A presence-bound room key is visible only while the room is open at its
    generation.
  - Persistent keys survive both.
  - After presence expires there is a rejoin grace period (default 30 s) before
    presence-bound keys are pruned.
- **Presence** is the explicit durable presence-bound key `member/<id>/presence`.
  It is never derived from activity. Its value is `ver u8 = 1, state u8 (1 =
  online), since_ms u64, daemon_incarnation u64`, and an empty value means
  offline.
  - The writer re-sends it every `presence_interval` (5 s).
  - A receiver marks the writer expired `3 × presence_interval` after the last
    *arrival* of a newer presence version, measured by the receiver's monotonic
    clock.
  - A member never accepts a remote "offline" for itself. If it receives its
    own key at a version ≥ its own, it bumps past it and re-asserts.
- **Late join.** A peer sends its durable records as a snapshot (`SnapshotBegin`,
  records, `SnapshotEnd`). Deltas follow on the same ordered lane, so everything
  after the cut is strictly newer. Local subscribers get `reset` and then the
  snapshot.
- **Forwarding.** Durable records are flooded: a node forwards a record that
  improved its own state to its other peers. Live records go direct only.

## 4. Peer connection and hello (frozen in R0)

- **One connection per peer** for room traffic, under ALPN `prim/1`. When two
  connections exist for the same peer (a simultaneous dial), each side keeps the
  one dialled by the lower key and closes the other with `duplicate`.
- **Lanes** are QUIC streams under **strict priority**:
  - lane 0: control (hello, records, presence, acks). This is the bi stream the
    dialer opens. Priority 2.
  - lane 1: state and messages. One uni stream per direction, opened lazily.
    The first byte is the lane ID. Priority 1.
  - lane 2: bulk (reserved, file streams). Priority 0.
  - Datagrams ride on the same connection.
- **Frames:** `u32 length (type + body, ≤ 1 MiB) || type u8 || body`.

| type | name | body |
|---|---|---|
| 0x01 | hello | `magic "prim" version u16 caps u64 room_id[32] member[32] daemon_incarnation u64 open_generation u32 proof_count u8 (bytes16 record) × n` |
| 0x02 | reject | `reason u16 detail bytes16` |
| 0x10 | records | `count u16 (bytes16 record) × n` |
| 0x11 | snapshot_begin | `count u32` |
| 0x12 | state | `bytes16 state record` |
| 0x13 | snapshot_end | — |
| 0x14 | records_end | — (ends the initial `records` sync; receivers decide membership only after it) |
| 0x20 | message | `msg_id u64 channel u16 flags u8 target u8 payload bytes32` |
| 0x21 | message_ack | `msg_id u64 status u8` |
| 0x30 | goodbye | `reason u16` |

- **Hello.**
  - `version` must be 1. A peer with any other version is rejected with `version`.
  - `caps` bits 0–31 are optional and unknown ones are ignored. Bits: 0 state,
    1 messages, 2 datagrams, 3 streams, 8 voice (the peer accepts datagram kinds
    2 and 3; whether anyone listens is its `member/<id>/voice` key, section 6).
  - `caps` bits 32–63 are **required**: a receiver that sees a required bit it
    doesn't implement rejects with `unsupported`. Version 1 defines none, so a
    future wire extension that old peers must not misread sets one instead of
    bumping `version`.
  - `member` must equal the transport's remote key.
  - `proof` carries the invite redemption (section 7).
- **Reject and goodbye reasons:** 1 version, 2 wrong_room, 3 not_member, 4 closed,
  5 revoked, 6 duplicate, 7 invite_invalid, 8 shutdown, 9 leave, 10 unsupported.
- **Message `target`:** 0 the receiver, 1 broadcast copy, 2 coordinator. A
  coordinator message is resolved when sent and fails locally with
  `no_coordinator` when there is none.
- **Message ack status:** 0 delivered, 1 no_channel, 2 not_member, 3 too_large.
- **Messages are dropped by default.** They are never queued for an offline
  member, and a message to a channel no local app registered is dropped and acked
  `no_channel`.
- Membership never grants a channel.

## 5. Datagram header (frozen in R0)

`ver_kind u8` (high nibble version = 1, low nibble kind), then the kind body.
Decoders are strict (section 0); an unknown kind or version is dropped and counted.

| kind | name | body |
|---|---|---|
| 0 | live state | an unsigned live state record (section 3) |
| 1 | app | `channel u16 seq u16 send_ms u32 payload` |
| 2 | voice | the voice envelope (section 6) |
| 3 | viseme | the viseme body (section 6) |
| 4–15 | reserved | dropped |

- `send_ms` is the sender's monotonic milliseconds, wrapping at u32.
- Receivers compute age only as a delta against the smallest observed
  `(arrival − send_ms)` for that peer. That needs no shared clock.
- The daemon enforces `max_age` in its own send queue, because iroh drops
  datagrams on buffer overflow, not by age. Voice has no `send_ms`: its age bound is
  local (the send queue), and its jitter is NetEq's job from the 48 kHz timestamp.
- Never assume an MTU: read `max_datagram_size()`. The floor is about 1.2 KB. The
  largest live record (a quantized pose with a 48-byte name and 200-byte value) is
  305 B with the header, and the largest voice datagram is 1015 B (`golden.rs`
  `datagrams_fit_the_floor`).
- Datagrams are never forwarded. The sender of every kind is the connection's
  authenticated remote key.

## 6. Voice envelope (frozen in R0)

**Datagram kind 2.** After `ver_kind = 0x12`:

| offset | size | field |
|---|---|---|
| 1 | 1 | `voice_ver`: high nibble envelope version = 1, low nibble codec (1 = Opus, 48 kHz, mono; others rejected) |
| 2 | 4 | `incarnation` u32: random, nonzero, fresh per stream start, never reused by a sender |
| 6 | 2 | `seq` u16: +1 per packet, wraps |
| 8 | 4 | `timestamp` u32: 48 kHz sample index of the packet's first sample, wraps |
| 12 | 1 | `flags`: bit0 talkspurt start, bit1 talkspurt end, bit2 FEC included, bit3 stream start (set on the first five packets of an incarnation); bits 4–7 zero |
| 13 | 2 | payload length `n`, 1..=1000 |
| 15 | n | one Opus packet |

- **Sender binding.** There is no sender field. The sender is the authenticated
  remote key of the connection, which the hello bound to a member of this room at
  this open generation. A receiver accepts voice only on an established connection
  from a current member, and only while a local client holds its `voice` lease;
  otherwise it drops and counts.
- **Incarnation.** A restarted voice client starts a new incarnation; a device
  change does not (it keeps the incarnation and the sample clock, advancing
  `timestamp` over the gap). Receivers keep, per sender, the current incarnation and
  the last 8 retired ones (`voice::IncarnationFence`):
  - the current incarnation: accept;
  - a retired one: drop (a late packet of a dead stream);
  - anything else: a new stream. Retire the current one, reset jitter-buffer and
    decoder state, accept.
  Incarnations are random, so there is no "newer" comparison, and a restart is
  never mistaken for reordering of the old stream. `seq` and `timestamp` are
  compared only within one incarnation.
- **Timestamps** start at any value (R0's sender starts at 0) and advance by the
  samples each packet covers, including over silence and device gaps, so NetEq sees
  one continuous clock per incarnation.

**Datagram kind 3, visemes.** After `ver_kind = 0x13`: `viseme_ver u8 = 1,
incarnation u32 (nonzero, the voice stream described), sample_pos u32 (48 kHz voice
timestamp at the analysis window's centre), count u8 ≤ 32, weights u8 × count`.
A receiver aligns visemes to audio by comparing `sample_pos` with the timestamp it
is playing out for that incarnation (a local playout observation, VOICE.md), and
drops visemes whose incarnation isn't the sender's current one. R0 defines the
format; nothing sends it yet.

**`member/<id>/voice`.** Durable, presence-bound, written only by the member's
daemon from its local voice lease: `ver u8 = 1, flags u8 (bit0 listening, bit1 mic),
incarnation u32` (the incarnation being sent, 0 if none). Listening means a voice
client holds the lease and renders received voice; when the lease ends the daemon
writes `flags = 0`, so friends can see who can hear them.

## 7. Invite token

Text: `prim1` followed by unpadded lowercase RFC 4648 base32 of:

```
version u8 = 1
room_id[32]
invite record bytes16       (kind 4, subject = ephemeral key E)
ephemeral_secret[32]        (E's Ed25519 secret)
peer_count u8, peer[32] × n (bootstrap member keys, n ≤ 4, inviter first)
relay_count u8, bytes8 url × m   (relay hints, m ≤ 2, ≤ 100 B each)
check[4] = first 4 bytes of H("prim/1 token-check", all bytes above)
```

**Redemption.** The joiner `J` signs a member grant (issuer `E`, subject `J`) with
the ephemeral secret. It sends `[invite, member]` as the hello proof to any
bootstrap peer. That peer admits `J` only if all of these hold:
- the invite is valid in its set;
- `E` has issued no other member grant;
- the room is open at `invite.generation`;
- `invite.expiry_ms` is later than the admitting peer's wall clock.

Then it floods both records.

Expiry and generation binding are enforced only by the admitting peer. Records
that arrive by gossip are judged by set validity, which has no clock and ignores
the invite's generation. So these bind honest members only: a member holding an
unused token can still redeem it after expiry or a reopen and flood the result.
Under the v1 threat model (invited friends) that is accepted; closing the room,
or revoking the invite key, is the hard stop. Closing the room therefore kills every outstanding
token (a reopen has a new generation), and the token is spent once its member
grant is known. A member that never uses a token can lose it to a thief.
Equivocation then voids both admissions, so the theft is visible.

Lost root key: keyholders can still reopen and admit, but root can't be
recovered or rotated in v1. The UI must say so.

## 8. Identities, incarnations and time

| identity | type | scope |
|---|---|---|
| room | `room_id` (32 B) | forever |
| open generation | u32 ≥ 1 | one open…close span |
| member | Ed25519 key = iroh `EndpointId` | forever |
| member incarnation | u32, persisted counter | one daemon start; the member-scope `term` |
| daemon incarnation | u64, random per process | in hello and presence |
| client session | u64, per local API connection | provisional local API |
| voice stream incarnation | u32, random nonzero | one voice stream (a voice client start); in the voice envelope, visemes and `member/<id>/voice` |
| local lease session | u64, per realtime-socket session | provisional local API (VOICE.md) |
| coordinator epoch | `(generation u32, term u32)` | per claim |

Time:
- **On the wire:** `*_ms` fields are Unix milliseconds from the signer's wall clock.
  They are informational, except that expiry is compared against the
  *checking* node's wall clock at admission. Datagram `send_ms` is
  sender-monotonic and is used only as a delta. The voice `timestamp` is the
  sender's 48 kHz sample clock, meaningful only within one incarnation.
- **On one host:** the daemon and its local clients share one clock domain,
  `mono_us` = CLOCK_MONOTONIC in microseconds. `received_at_mono_us` (stamped at the
  daemon's datagram receive) and pose `source_mono_us` are in it, so a client
  subtracts them from its own CLOCK_MONOTONIC directly. A client that sees a stamp
  in the future or more than 1 s old treats the domains as mismatched, uses its own
  read time instead and counts it.
- **In the core:** time is an input, `Now { mono_us: u64, unix_ms: u64 }`.
  `room-core` never reads a clock. Presence expiry, coordinator liveness and grace
  periods use `mono_us` at the *receiver*.
- **Wraparound:** `seq` is u64 and never wraps. `send_ms` and datagram `seq` are
  u16/u32 and are compared with wrapping subtraction as signed.

Defaults: presence interval 5 s, presence expiry 15 s, coordinator liveness 20 s
("unresponsive"), rejoin grace 30 s.
