# Room continuity, restoration, and eventual moderation

Planning proposal, 2026-09-16. No runtime changes are implemented by this note.
Based on the current working tree, including the uncommitted participant-file
sharing work. This extends the party lifetime described in
[experience continuity](EXPERIENCE_CONTINUITY.md).

## Product decisions from this discussion

- A room belongs to its participants. Losing its current coordinator must not
  end everyone else's session.
- Both sides of a temporary partition remain usable. After reconnecting they
  converge; a playback seek or source change is acceptable.
- Participants cache room state under the lobby identity. A participant can
  restore an empty room, and other cached replicas reconcile when they return.
- Moderation passes to the oldest currently participating person. The room
  creator does not retain a special permanent moderator role.
- Basic bans against a locally stored participant key are sufficient initially;
  creating a new identity can evade them.
- Bans made by either group during a partition survive reunion until the
  reunited room's moderator explicitly lifts them.
- Initial scale remains the existing six-person mesh. High churn, strong
  consensus, and adversarial distributed agreement are not initial targets.

The moderation section distinguishes these product choices from the authority
validation and discovery changes still needed before implementing a public beta.

## What already exists

| Area | Current evidence | Consequence |
| --- | --- | --- |
| Transport | `native/src/network.rs` establishes a full mesh; pose and voice broadcast directly | Surviving participants already have their own transport paths |
| Host failure | The disconnect handler clears the host and calls `state.stop()` | This deliberately ends the session and media worker; replacing this is necessary but insufficient |
| Membership | Only the host publishes `Wire::Members`; recipients use it to connect | Every member needs enough room information to bootstrap and repair the mesh |
| Discovery | One address; 90-second application expiry; roughly 20-second republish loop; discovery returns after joining | Backups and continuous discovery/repair need new lifecycle logic |
| Playback | `project/media/playback.gd` has source generation, revision, periodic samples and four-timestamp probes | Useful migration foundation, but no coordinator epoch or replicated durable document |
| Playback transition | `host_changed()` resets clock probes and publishes the new host's local player position | A buffered or unloaded successor must instead recover the shared timeline |
| File sharing | Provider and playback host are already separate; offer/activate/ready commits a source | Preserve the provider and active transfers when only the coordinator changes |
| UI cleanup | Finished native worker emits `left_room`; `project/main.gd` mutes/removes everyone | Failover must emit a coordinator transition, not room departure |
| Identity | Endpoint is created afresh; room-secret proof authenticates admission | Persistent participant identity and process incarnation are separate future needs |

The lockfile currently resolves Iroh 1.2.0 and pkarr 5.0.3. Examples from newer
documentation are design references, not verified drop-in dependency changes.

## Recommended structure

Use a replicated room document with a replaceable live coordinator. All members
hold state; the coordinator is a convenience for playback timing, the existing
file-offer transaction, and ordinary discovery publication. It is not the room's
identity or the only surviving copy of its state.

| State | Representation / lifetime |
| --- | --- |
| Room identity | Stable `RoomId`, independent of endpoint or coordinator; derived with domain separation from the current high-entropy lobby capability |
| Participant identity | Locally persisted public/private key; no account service required; necessary before durable identity-based moderation |
| Live participation | Fresh incarnation per join after leaving/restarting, addresses, join rank and expiring presence; not restored as live membership |
| Room content | Persisted CRDT document: playback intent initially; stable object IDs, placement and asset references later |
| Coordinator | Ephemeral selection plus a versioned coordinator epoch; no quorum or global exclusivity guarantee |
| Playback observations | Intent ID, coordinator epoch, sequence, position and monotonic timestamp; ephemeral |
| Voice / tracking / manipulation preview | Existing datagrams or other bounded transient channels; no document history |
| Media / asset bytes | Separate transfer and cache lifetime; document replication does not replicate bytes |
| Moderation policy | Separately validated signed actions and identity rules; never an unrestricted writable `is_admin` property |

Keep these modules inside the current process. There is no need to build the
resident service discussed in the experience plan to implement room continuity.

## Selection and failure detection

Wall-clock join time can provide an approximate preference among trusted peers.
Clock error would usually just pick a different person. It does not solve stale
membership, failed candidates, disconnected views, or delayed messages, though.
A replicated logical join rank costs little and avoids depending on NTP:

1. When admitting a new live incarnation, the current coordinator allocates
   `max_observed_join_counter + 1`, paired with a unique incarnation ID to break
   ties. Replicate the admission and high-water mark. Concurrent first joins or
   admissions in separate partitions can tie on the counter; the ID breaks ties.
2. Retain the rank through a short transport reconnect. Explicit leave or process
   restart is a new live join. A cached document does not make its owner an old
   online participant. The counter preserves causal admission order, not a
   universal real-time order across partitions.
3. Choose the smallest eligible live rank in the locally reachable membership
   view. A graceful leave can advertise a handoff; a crash uses the same path
   after timeout. A successor need not see a majority: one survivor can continue.
4. Use monotonic liveness timers and a short stabilization interval. Detect the
   ability to process room control, not just an open QUIC socket. A stalled Godot
   playback loop with a healthy network worker must not hold coordination forever.
5. Exchange membership and state summaries before issuing the successor's first
   snapshot, with a bounded wait for responsive peers. Skip failed candidates.
6. Announce a fresh coordinator epoch, ordered by a logical counter plus unique
   issuer incarnation. Tag clock probes, samples and pending file transactions
   with it. Adopt only the selected candidate; a high epoch alone is not proof
   that someone is eligible. Retire old epochs and deduplicate commands by ID.

An epoch is a stale-message boundary among peers that have adopted it, not a
globally exclusive lease. Partitioned groups can have different coordinators.
Once membership views and connectivity stabilize, rank comparison must make
them select the same candidate. The selected candidate issues an epoch above
the observed epochs; losers yield without endlessly increasing their counters.
The state-machine tests must establish this, including asymmetric partial meshes.

The proposed simple reconnect rule is that a still-valid older incarnation may
retake coordination after stabilization; a restarted participant gets a new rank.
Add hysteresis to avoid rapid switching. Exact timer values should come from the
failure-injection harness, not be treated as protocol guarantees in this plan.

Keep selection separate from permission checks even if the same oldest person
normally coordinates and moderates. An overloaded playback client can stop being
the clock source without silently changing a person's moderation privileges.
Define live membership expiry consistently for moderation before public release.

## Durable state and CRDT choice

Restoring an empty room makes a small CRDT integration worthwhile early. Prefer
a bounded Rust Automerge document over the existing authenticated Iroh mesh.
The [Iroh Automerge example](https://docs.iroh.computer/protocols/automerge)
demonstrates transport integration; production lifecycle, persistence and
authorization remain application work. The
[Rust sync protocol](https://automerge.org/automerge/automerge/sync/index.html)
uses a reliable ordered stream with separate sync state for each peer.

Use a separate bounded sync stream so history catch-up cannot fill the existing
small control queue or delay voice. Enforce document, frame and work limits and
backpressure. Replicate through any surviving peer, not just the coordinator.

- Cache documents by `RoomId`; save crash-safely and restore the actual CRDT
  history, not just a materialized JSON view. Scope secrets and local paths out
  of replicated state. Keep cache operations independent of lobby discovery.
- Define a canonical initial schema/base document so independent first opens do
  not create conflicting root containers. Give independent writers unique actor
  identities; a copied cache must not cause concurrent reuse of one actor sequence.
- Sync missing changes on join, reconnect and digest mismatch. Do not overwrite
  a remote document with a cached snapshot. A discovery failure means isolation,
  not proof that no other replica is running.
- Persist before presenting an edit as saved locally. An edit that reached no
  surviving replica can still be lost if its only local storage is lost. There
  is no always-online backup promise.
- Preserve deletion history/tombstones for returning replicas. Saving a compact
  file is not permission to discard causal history. A hard history reset needs
  an explicit document-generation/migration policy so stale caches cannot revive
  deleted objects. Measure history growth before selecting a retention policy.

For playback, use one atomic intent value: source identity, play/pause/seek intent,
position anchor and unique intent ID. Concurrent edits must select an entire
intent, never combine one video's URL with another video's position. Automerge
provides deterministic conflict selection and exposes alternatives; it does not
promise human wall-clock last-write order. See its
[conflict semantics](https://automerge.org/docs/reference/documents/conflicts/).

Periodic clock samples and coordinator changes do not create playback edits or
win conflicts merely by being frequent. The coordinator schedules the document's
selected intent. Initially retain the existing source-offer coordinator path;
there is no need to make every media transaction leaderless in the first pass.

For future objects: stable random IDs, independent properties where combination
makes sense, one atomic transform where it does not, and an explicit delete/edit
policy. Send drag/physics previews transiently, then persist the final placement.
Physics simulation and exclusive grabbing are separate problems from storing
object placement. Asset hashes/references live in the document; asset contents
and availability do not.

Alternatives considered:

| Option | Fit here |
| --- | --- |
| Small custom versioned snapshot | Shortest host-survival patch; inadequate by itself for offline room edits and cached-prop reconciliation |
| Automerge over existing mesh | Recommended first spike: structured state and history without changing Prim's peer topology |
| `iroh-docs` | Good candidate for signed per-author key/value records and content-hash storage; application still defines cross-author object semantics |
| `iroh-gossip` | Useful if the six-person full mesh becomes a scaling problem; does not replace persistence, admission or moderation |
| Full Matrix protocol | More federation and authorization machinery than this room needs; borrow separation of membership, permissions and ordinary content |

[Iroh documents](https://docs.iroh.computer/protocols/documents) index entries by
namespace, author and key and combine docs, blobs and gossip. That is useful, but
not automatically a shared JSON object graph or a revocable moderation model.
[Iroh gossip](https://docs.iroh.computer/connecting/gossip) still needs bootstrap
peers. Neither choice eliminates the discovery problem.

## Playback and provider continuity

During failover, keep each surviving client's player, voice, mute choice, avatar
instances and healthy media connections alive. Briefly hold or bound new control
requests while choosing a coordinator; use request IDs so retries cannot apply a
toggle or relative seek twice.

The successor recovers the selected playback intent and its mapped timeline from
surviving replicas. It must not blindly publish a local buffering position or an
unloaded player's pause flag. Convert the old sample through the known offset to
local monotonic time, extrapolate if playing, and re-anchor against the successor's
clock. Invalidate the old clock offset/probes and obtain fresh samples. If the old
mapping is unavailable, make one explicit bounded resynchronization instead of
subtracting timestamps from unrelated process clocks.

On partition reunion, merge document state first, choose the playback intent,
then re-anchor its timeline. A corrective seek or source replacement is allowed.
Late packets from the prior coordinator must not switch it back.

| Failure | Expected behavior |
| --- | --- |
| Coordinator leaves; movie is a URL | Room continues; successor restores synchronization |
| Coordinator leaves; another peer supplies the file | Preserve the same publication and receiving sessions |
| File provider leaves | Room/voice survive; show source unavailable or stop this movie |
| Coordinator was also the only file provider | Room survives; missing bytes cannot be recovered by election |
| Coordinator dies during offer/activate/ready | Abort/retry the unfinished transaction under the new epoch; preserve the last committed source |
| Everyone exits, then someone restores | Load cached room content; playback starts paused at the saved anchor/checkpoint by default |

A client initially displays its cache paused without writing a new shared pause
operation. If it discovers an active room, sync and adopt that room's live
timeline. If it remains isolated, resume only on an explicit play action. This
avoids a returning stale cache overwriting an ongoing movie merely by opening.
Checkpoint cadence and retained playback history can be modest; do not persist
the one-second observation stream just to support resuming a movie later.

Restoration must not turn a stale file-share descriptor into a live permission to
read a local path or upload bytes. Require a fresh provider publication. Preserve
relay consent and bandwidth controls. Existing movie caching remains a bounded
temporary cache; persistent prop/image/model assets need a separate explicit
storage policy. Restored asset references can initially be placeholders until a
participant with the bytes reconnects.

## Discovery and repairing splits

Publish a compact record containing several useful bootstrap participants, e.g.
the coordinator plus two successors. Any authenticated member can introduce a
joiner to the mesh and return the current room view; joining a backup must not
require that backup to claim to be host.

Pkarr is a replaceable signed DNS packet, not an append/merge data structure.
The pinned [SignedPacket API](https://docs.rs/pkarr/5.0.3/pkarr/struct.SignedPacket.html)
limits encoded DNS payload to 1000 bytes. Several verbose JSON addresses with
IPv6 and relay URLs may exceed this: size the encoded packet and cap address
hints, using compact endpoint IDs and relay information preferentially.

Start with one publisher per connected group, normally its coordinator. That
publisher includes the backups; successors take over publication. Redundant
addresses do not require every backup to race to publish a different packet.
The pkarr 5.0.3 source supports read/modify/publish with CAS and retry; use it to
reduce lost updates, without treating per-DHT-node CAS as a global lock.
[BEP 44](https://www.bittorrent.org/beps/bep_0044.html) specifies the per-node
sequence/CAS behavior. Carry the last observed publication sequence across
handoff and advance beyond it; do not assume the successor's wall clock is newer.

Keep a low-rate resolve/repair loop active after joining. Try cached participants
and multiple discovered addresses with bounded concurrent dialing, backoff and
deduplication. During a split, publishers may overwrite one another's hints;
resolving as well as publishing lets a group discover and contact the other.
Once connected, exchange membership and CRDT state, settle coordination, then
publish the combined live bootstrap set. No publication success is evidence of
exclusive ownership. Existing rooms must work while the DHT is unavailable.

Independent fixed discovery slots could reduce write collisions if measurements
justify them, but introduce their own assignment, expiry and lookup costs. They
are unnecessary for the initial single-publisher-with-backups version.

After everybody leaves, DHT records may expire. A returning client has the room
identity and its local document, can republish a live endpoint, and later merges
with returning replicas. A completely new client cannot recover state while every
cached replica is offline; an always-on seed would be an optional later service.

## Oldest-participant moderation and public testing

The user's policy is transferable seniority, not permanent creator ownership.
The moderator is the oldest eligible live participant; a new live session does
not regain seniority just because its device has old room history. Cached bans
can persist while the moderator role itself is recomputed from live membership.

Keep the concepts separate even when one person fills both roles. VRChat makes a
similar distinction between network master and instance owner, although its
current documentation does not guarantee oldest-player master succession. See
[ownership](https://creators.vrchat.com/worlds/udon/networking/ownership/) and
[network properties](https://creators.vrchat.com/worlds/udon/networking/network-components/).
Prim's requested moderation succession is its own policy.

Before public testing:

- Persist a participant key; bind the current endpoint/incarnation to it with a
  proof. Local block hides/ignores a person for this viewer. Room kick ends an
  incarnation; room ban denies a participant identity until explicitly lifted.
- Validate moderation on every peer, including media admission and state sync.
  Dropping the moderator's one connection to an offender does not remove the
  offender from the full mesh. Include bounded admission and sync resource limits.
- Do not accept self-reported ancient join timestamps as moderation credentials.
  Admissions need verifiable identity and a recorded ordering basis. A current
  moderator can authenticate admission order while online, but crash succession
  and competing membership views still need explicit rules.
- Keep signed policy events separate from arbitrary Automerge document edits.
  An actor ID is not an authentication signature. Record event identity, issuer,
  affected identity/incarnation and the membership/policy context it refers to.
- The existing lobby secret derives the pkarr private key. Anyone given that
  secret can rewrite discovery, even after a ban. Separating derivation labels
  does not revoke a key already known to a participant.

For a controlled public beta, choose how discovery is administered before
claiming effective room bans: trusted discovery publishers/seed, a replaceable
invitation with a new discovery key, or a discovery design in which invitees do
not receive the shared signing key. A public read/join ticket should not silently
grant permanent directory-writing power. This is a separate availability issue
from rejecting a banned identity on an authenticated connection.

There is a real limit to the desired oldest-online rule: during a partition, a
peer cannot prove that older members are offline elsewhere. If both groups are
allowed to continue, they can temporarily have different moderators. Signatures
can prove who issued an action; they cannot prove a remote participant was dead.
A malicious peer can also claim isolation. Strong adversarial succession requires
a stronger admission/authorization model or sacrificing some availability.

For the agreed basic cooperative beta, retain verified ban events from either
branch and require an explicit later unban that observes those events.
Concurrent unban must not erase an unseen ban. However, "verified" must include
the accepted authority context, not just a valid signature. Conflicting bans of
the competing moderators themselves need a specified resolution/recovery rule.
Do not implement this as an ordinary last-writer-wins admin flag. Represent each
ban with a unique event ID and have an unban explicitly retire observed ban IDs;
merge by retaining every ban without a valid retirement. This defines the merge
semantics, not by itself the authorization to issue either operation.

The proposed default after reconciliation is to choose the oldest non-banned live
participant. If mutually valid bans leave no eligible moderator, do not silently
self-unban: make recovery explicit, such as creating a new room identity from a
saved content copy. Finalizing authority validation and this exceptional recovery
flow belongs to the moderation step, before enabling it for public testers.

Matrix is useful inspiration for validating permission-changing events against
their authorization context before resolving room state; see the
[room authorization/state-resolution specification](https://spec.matrix.org/v1.18/rooms/v12/).
It does not supply Prim's oldest-live-participant policy or eliminate its partition
tradeoff. Identity resets also evade a key-only ban unless admission is restricted.
This is an explicitly bounded moderation feature, not Sybil-resistant identity.

## Implementation sequence and acceptance gates

1. **Model and storage spike.** Add a small testable room-state reducer and a
   bounded Rust Automerge prototype using the existing Iroh endpoint. Prove
   independent initialization, playback-value conflicts, serialization/restart,
   and three-replica partition/rejoin. Finalize version/counter encodings that
   round-trip through Godot JSON without integer precision loss.
2. **Live room continuity.** Replace host-loss teardown; replicate membership,
   logical join order and coordinator epochs. Add liveness, mesh repair and
   bounded transition handling. Adapt playback probes, samples and file-offer
   transactions. Keep the existing code/provider separation.
3. **Discovery and restoration.** Publish multiple bootstrap peers, take over
   publication, continue resolving while connected, and persist/sync room state.
   Qualify simultaneous startup and reunion after all participants restart from
   different cached revisions. This completes the requested continuity behavior.
4. **Moderated public beta.** Specify identity, admission, ban/rejoin behavior,
   authority-context validation and discovery-key handling; then implement the
   agreed bounded moderation policy. This can precede a general UGC system.
5. **First persistent object.** Add one placeable image with stable ID, final
   transform, deletion and content-hash reference. Prove missing-asset behavior
   and offline restoration before introducing arbitrary model import or physics.

Suggested implementation boundaries: a native room module for membership,
coordinator selection, document persistence/sync and validation; `network.rs` for
transport/discovery; `session.rs` for room/coordinator events; playback script for
mpv observation and application of the selected room intent. Audit media lifetime
checks rather than restarting all media on a coordinator change. Bump the room
protocol and reject incompatible builds clearly.

Required tests include:

- Three and six peers; graceful leave, process kill and blackholed coordinator;
  only one survivor; successor failing during recovery; late join through backup.
- Surviving voice/pose paths and mute state remain intact; no `left_room` event or
  healthy-player reload on ordinary coordinator loss. Measure perceived audio
  separately from native packet delivery.
- URL, non-coordinator file provider and coordinator-as-provider cases; pending
  offer failure; provider unavailable to the successor; delayed old file messages.
- Delayed/duplicated old-epoch messages, failed control streams with a live socket,
  main-thread stall, sleep/resume, wall-clock jumps, and transient partial meshes.
- 3/3 and 1/5 splits with edits on both sides; merge in different delivery orders;
  no perpetual coordinator oscillation or repeated playback reset after healing.
- Two independent first starts and two independent cached restores; old replica
  returning after a deletion; truncated/corrupt local save; stable schema roots.
- Stale DHT data, publication collisions, DHT unavailable after initial bootstrap,
  and maximum encoded record size with realistic IPv6/relay addresses.
- For moderation: forged seniority, banned reconnect, identity reset, partition
  ban/unban and mutual-moderator bans, unauthorized policy edits, discovery rewrite.

Use deterministic reducer tests and a local fault-injection integration harness
first, then public DHT/WAN/relay and Linux/Windows/headset checks. Do not infer live
WAN convergence or uninterrupted audible voice from a passing local state test.
