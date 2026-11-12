//! The room state machine (ROOM.md). Sans-IO: the driver feeds [`Input`]s with the
//! current [`Now`] and drains [`Output`]s. No clock, no socket, no OS RNG, no threads;
//! every nonce comes from [`Entropy`] and every iteration order is deterministic
//! (`BTreeMap`), so seeded simulator runs replay exactly.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::api::*;
use crate::frame::{
    msg_flags, AckStatus, Datagram, Frame, Hello, Reason, Target, PROTOCOL_VERSION,
};
use crate::grants::{AdmitError, Epoch, GrantSet, InsertError, Lifecycle};
use crate::ids::{Entropy, Identity, Key, Now, RecordId, RoomId};
use crate::record::{build, Kind, Record};
use crate::state::{is_presence, Insert, PresView, Slot, Store, Vis};
use crate::staterec::{
    valid_name, Class, Lifetime, Presence, Scope, StateRecord, Version, DURABLE_VALUE_CAP,
    LIVE_VALUE_CAP, PRESENCE,
};
use crate::subs::{Pattern, Subs};
use crate::token::{InviteToken, PEER_CAP, RELAY_CAP, RELAY_LEN_CAP};
use crate::wire::{h, DecodeError, Reader, Writer};

/// Largest accepted message payload (ROOM.md "Messages and datagrams").
pub const MESSAGE_CAP: usize = 64 * 1024;
const ORPHAN_CAP: usize = 256;
/// State records a joiner holds before its grant set arrives.
const PRE_GRANT_CAP: usize = 4096;
/// Frames and bytes held per connection before its hello completes.
const EARLY_FRAMES: usize = 4096;
const EARLY_BYTES: usize = 4 << 20;
const RECORDS_PER_FRAME: usize = 64;
const DIAL_TIMEOUT_US: u64 = 15_000_000;

/// What the daemon saves to disk and hands back to [`Room::resume`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Persisted {
    pub room: RoomId,
    /// Signed records (genesis first), wire bytes.
    pub records: Vec<Vec<u8>>,
    /// Durable persistent state records of every writer, wire bytes.
    pub state: Vec<Vec<u8>>,
}

const PERSIST_MAGIC: [u8; 4] = *b"PRSP";
const PERSIST_VERSION: u8 = 1;

impl Persisted {
    /// Strict simple format: `"PRSP" version u8=1 room[32] n u32 (bytes16 record)×n
    /// m u32 (bytes16 state record)×m`, little-endian, no trailing bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.raw(&PERSIST_MAGIC).u8(PERSIST_VERSION).raw(&self.room.0);
        w.u32(self.records.len() as u32);
        for r in &self.records {
            w.bytes16(r);
        }
        w.u32(self.state.len() as u32);
        for r in &self.state {
            w.bytes16(r);
        }
        w.finish()
    }

    pub fn decode(bytes: &[u8]) -> Result<Persisted, DecodeError> {
        let mut r = Reader::new(bytes);
        if r.array::<4>()? != PERSIST_MAGIC {
            return Err(DecodeError::Magic);
        }
        let v = r.u8()?;
        if v != PERSIST_VERSION {
            return Err(DecodeError::Version(v));
        }
        let room = RoomId(r.array()?);
        let mut list = |what: &'static str| -> Result<Vec<Vec<u8>>, DecodeError> {
            let n = r.u32()? as usize;
            if n > 1 << 20 {
                return Err(DecodeError::TooLarge(what));
            }
            let mut v = Vec::with_capacity(n.min(1024));
            for _ in 0..n {
                v.push(r.bytes16(u16::MAX as usize, what)?.to_vec());
            }
            Ok(v)
        };
        let records = list("records")?;
        let state = list("state")?;
        r.end()?;
        Ok(Persisted {
            room,
            records,
            state,
        })
    }
}

/// Counters for things the core dropped. Exposed for tests and the daemon's logs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub bad_frames: u64,
    pub bad_records: u64,
    pub rejected_records: u64,
    pub bad_state: u64,
    pub rejected_state: u64,
    pub over_cap_state: u64,
    pub orphans_dropped: u64,
    pub dropped_datagrams: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CState {
    /// Hello exchange in progress.
    Handshake,
    /// This node is closed and the peer claims a newer open generation; waiting for
    /// its records to prove it.
    Probation,
    Established,
}

struct Conn {
    peer: Key,
    outbound: bool,
    state: CState,
    deadline_us: u64,
    /// Grant records received during the peer's initial sync, held until its
    /// `RecordsEnd` so membership is judged on the whole set. `None` once the sync
    /// ended (later `Records` frames are incremental floods).
    sync: Option<Vec<Record>>,
    /// State-lane frames that overtook the hello (the lanes are separate streams):
    /// held, bounded, and replayed once the connection is established.
    early: Vec<(Lane, Vec<u8>)>,
    early_bytes: usize,
}

#[derive(Default)]
struct DialState {
    next_us: u64,
    in_flight_until: Option<u64>,
}

#[derive(Default)]
struct Track {
    last_arrival_us: u64,
    /// When the last connection to this peer dropped (cleared on reconnect).
    disc_at_us: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PStatus {
    Unknown,
    Online,
    Offline,
    Unresponsive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CoordStatus {
    Fresh,
    Left,
    Unresponsive,
}

struct CoordState {
    epoch: Epoch,
    holder: Key,
    adopted_us: u64,
    status: CoordStatus,
}

struct Joining {
    token: InviteToken,
    proof: [Record; 2],
}

pub struct Room {
    cfg: Config,
    me: Identity,
    my_key: Key,
    entropy: Entropy,
    now: Now,
    room: RoomId,
    grants: Option<GrantSet>,
    joining: Option<Joining>,
    out: VecDeque<Output>,
    conns: BTreeMap<ConnId, Conn>,
    by_peer: BTreeMap<Key, ConnId>,
    dial: BTreeMap<Key, DialState>,
    store: Store,
    orphans: VecDeque<StateRecord>,
    tracks: BTreeMap<Key, Track>,
    subs: Subs,
    published: BTreeMap<String, Vis>,
    clients: BTreeSet<ClientId>,
    channels: BTreeMap<u16, BTreeSet<ClientId>>,
    my_seq: u64,
    room_seq: BTreeMap<Epoch, u64>,
    my_since: Option<(u32, u64)>,
    next_presence_us: u64,
    roster_known: BTreeSet<Key>,
    pstatus: BTreeMap<Key, PStatus>,
    coord: Option<CoordState>,
    lifecycle_seen: Lifecycle,
    synced: bool,
    /// Derived from the grant set (and our own grant's expiry); recomputed whenever
    /// the set changes.
    removed: bool,
    /// The last `(generation, term)` this node claimed; never claim it twice.
    last_claim: Option<(u32, u32)>,
    /// A joiner's `SnapshotEnd` arrived before its grant set did.
    snapshot_end_early: bool,
    /// A claim that probed invalid: `(generation, term, grant set size)`, so the probe
    /// isn't repeated until the set changes.
    claim_probe_failed: Option<(u32, u32, usize)>,
    left: bool,
    next_msg_id: u64,
    pending_msgs: BTreeMap<(ConnId, u64), (ClientId, Key)>,
    dgram_base: BTreeMap<Key, u32>,
    dgram_seq: u16,
    dirty: Cell<bool>,
    stats: Stats,
}

fn cmd_req(cmd: &LocalCommand) -> Option<ReqId> {
    use LocalCommand::*;
    match cmd {
        Subscribe { req, .. }
        | Write { req, .. }
        | RegisterChannel { req, .. }
        | Send { req, .. }
        | Status { req }
        | CreateInvite { req, .. }
        | Grant { req, .. }
        | Revoke { req, .. }
        | Retire { req, .. }
        | Close { req }
        | Reopen { req }
        | Leave { req } => Some(*req),
        _ => None,
    }
}

fn admit_reason(e: AdmitError) -> Reason {
    match e {
        AdmitError::NotMember | AdmitError::Expired => Reason::NotMember,
        AdmitError::Closed => Reason::Closed,
        _ => Reason::InviteInvalid,
    }
}

impl Room {
    /// Sign as this node, citing the latest revocation of our own key so records we
    /// issue after a re-admission or our own retire aren't caught by the cascade.
    fn signer(&self) -> crate::record::Signer<'_> {
        let cites = self
            .grants
            .as_ref()
            .map(|g| g.latest_revocation_of(&self.my_key))
            .unwrap_or(RecordId::ZERO);
        crate::record::Signer::new(&self.me, cites)
    }

    // ------------------------------------------------------------------
    // Construction
    // ------------------------------------------------------------------

    fn blank(config: Config, now: Now, room: RoomId) -> Room {
        let me = config.identity.clone();
        let my_key = me.key();
        let entropy = Entropy::new(config.entropy_seed);
        Room {
            cfg: config,
            me,
            my_key,
            entropy,
            now,
            room,
            grants: None,
            joining: None,
            out: VecDeque::new(),
            conns: BTreeMap::new(),
            by_peer: BTreeMap::new(),
            dial: BTreeMap::new(),
            store: Store::new(),
            orphans: VecDeque::new(),
            tracks: BTreeMap::new(),
            subs: Subs::new(),
            published: BTreeMap::new(),
            clients: BTreeSet::new(),
            channels: BTreeMap::new(),
            my_seq: 0,
            room_seq: BTreeMap::new(),
            my_since: None,
            next_presence_us: 0,
            roster_known: BTreeSet::new(),
            pstatus: BTreeMap::new(),
            coord: None,
            lifecycle_seen: Lifecycle::Dead,
            synced: false,
            removed: false,
            last_claim: None,
            snapshot_end_early: false,
            claim_probe_failed: None,
            left: false,
            next_msg_id: 1,
            pending_msgs: BTreeMap::new(),
            dgram_base: BTreeMap::new(),
            dgram_seq: 0,
            dirty: Cell::new(false),
            stats: Stats::default(),
        }
    }

    /// Found a new room: genesis (this identity is root) plus `open(1)`.
    pub fn found(config: Config, now: Now, meta: &[u8]) -> Room {
        let mut entropy = Entropy::new(config.entropy_seed);
        let nonce: [u8; 16] = entropy.bytes();
        let genesis = build::genesis(&config.identity, nonce, now.unix_ms, meta);
        let room = genesis.room;
        let mut r = Room::blank(config, now, room);
        r.entropy = entropy;
        let mut g = GrantSet::new(genesis);
        let open = build::open(&r.me, room, 1, r.entropy.bytes(), now.unix_ms);
        if g.insert(open).is_err() {
            r.stats.rejected_records += 1;
        }
        r.grants = Some(g);
        r.synced = true;
        r.adopt_initial_view();
        r.settle();
        r
    }

    /// Resume a room this node belongs to from persisted state. It stays as it was
    /// (open or closed); a closed room needs `LocalCommand::Reopen` by a keyholder.
    pub fn resume(config: Config, persisted: Persisted, now: Now) -> Result<Room, DecodeError> {
        let mut it = persisted.records.iter();
        let genesis = Record::decode(it.next().ok_or(DecodeError::Short)?)?;
        if genesis.kind != Kind::Genesis || genesis.room != persisted.room {
            return Err(DecodeError::Field("genesis"));
        }
        let mut recs = Vec::new();
        for b in it {
            recs.push(Record::decode(b)?);
        }
        let mut g = GrantSet::new(genesis);
        let mut full = 0;
        for r in g.insert_all(recs) {
            match r {
                Ok(_) => {}
                Err(InsertError::Full) => full += 1,
                Err(_) => return Err(DecodeError::Field("record for another room")),
            }
        }
        let mut states = Vec::new();
        for b in &persisted.state {
            let s = StateRecord::decode(persisted.room, b)?;
            if s.class != Class::Durable {
                return Err(DecodeError::Field("persisted state class"));
            }
            states.push(s);
        }
        let mut room = Room::blank(config, now, persisted.room);
        room.stats.rejected_records += full;
        room.grants = Some(g);
        for s in states {
            // Orphans (writer no longer a member) are simply not restored.
            let _ = room.store.insert(s, room.grants.as_ref().unwrap());
        }
        room.adopt_initial_view();
        room.settle();
        Ok(room)
    }

    /// Join with an invite token: dial the bootstrap peers and redeem.
    pub fn join(config: Config, token: InviteToken, now: Now) -> Room {
        let mut r = Room::blank(config, now, token.room);
        // The member grant is a pure function of (token, joiner) so a restarted joiner
        // re-sends the same record instead of equivocating.
        let mut ctx = token.ephemeral.secret().to_vec();
        ctx.extend_from_slice(&r.my_key.0);
        let nonce: [u8; 16] = h("prim/1 join-nonce", &ctx)[..16].try_into().unwrap();
        let issued = token.invite.grant().map(|g| g.issued_ms).unwrap_or(0);
        let proof = token.redeem(r.my_key, nonce, issued);
        for p in &token.peers {
            if *p != r.my_key {
                r.dial.insert(*p, DialState::default());
            }
        }
        r.joining = Some(Joining { token, proof });
        r.settle();
        r
    }

    /// Prepare the derived view of a room constructed with a grant set.
    fn adopt_initial_view(&mut self) {
        let me = self.my_key;
        let g = self.grants.as_ref().unwrap();
        self.lifecycle_seen = g.lifecycle();
        self.roster_known = g
            .authority()
            .members
            .keys()
            .copied()
            .filter(|k| *k != me)
            .collect();
        for k in &self.roster_known {
            self.dial.entry(*k).or_default();
        }
        if let Lifecycle::Open(_) = self.lifecycle_seen {
            self.coord = g.coordinator().map(|(e, c)| CoordState {
                epoch: e,
                holder: c.holder,
                adopted_us: self.now.mono_us,
                status: CoordStatus::Fresh,
            });
        }
    }

    // ------------------------------------------------------------------
    // Public surface
    // ------------------------------------------------------------------

    pub fn handle(&mut self, now: Now, input: Input) {
        if now.mono_us >= self.now.mono_us {
            self.now = now;
        }
        match input {
            Input::Connected {
                conn,
                peer,
                outbound,
            } => self.on_connected(conn, peer, outbound),
            Input::Frame { conn, lane, bytes } => self.on_frame(conn, lane, bytes),
            Input::Datagram { conn, bytes } => self.on_datagram(conn, bytes),
            Input::Disconnected { conn } => {
                self.drop_conn(conn);
            }
            Input::DialFailed { peer } => {
                if let Some(d) = self.dial.get_mut(&peer) {
                    d.in_flight_until = None;
                }
            }
            Input::Tick => {}
            Input::Local { client, cmd } => self.on_local(client, cmd),
        }
        self.settle();
    }

    pub fn poll_output(&mut self) -> Option<Output> {
        self.out.pop_front()
    }

    /// Monotonic microseconds of the next deadline, if any.
    pub fn poll_timeout(&self) -> Option<u64> {
        if self.removed {
            return None;
        }
        let now = self.now.mono_us;
        let mut best: Option<u64> = None;
        // `due` reports a deadline even if it has already passed: work that a
        // `settle` has not yet acted on. Only deadlines that `settle` always consumes
        // may use it. `cand` skips past ones (an expired presence or liveness
        // threshold stays in the past forever and would spin the driver).
        let mut due = |t: u64| best = Some(best.map_or(t, |b| b.min(t)));
        for c in self.conns.values() {
            if c.state != CState::Established {
                due(c.deadline_us);
            }
        }
        if self.dialing_active() {
            for (k, d) in &self.dial {
                if self.by_peer.contains_key(k) {
                    continue;
                }
                match d.in_flight_until {
                    Some(t) => due(t),
                    None => due(d.next_us),
                }
            }
        }
        if let Some(t) = self.next_expiry_us() {
            due(t);
        }
        let mut cand = |t: u64| {
            if t > now {
                best = Some(best.map_or(t, |b| b.min(t)));
            }
        };
        if self.presence_active() {
            cand(self.next_presence_us);
            let t = &self.cfg.timing;
            let (exp, grace, live, intv) = (
                t.presence_expiry_ms * 1000,
                t.rejoin_grace_ms * 1000,
                t.coordinator_liveness_ms * 1000,
                t.presence_interval_ms * 1000,
            );
            for tr in self.tracks.values() {
                cand(tr.last_arrival_us + exp);
                cand(tr.last_arrival_us + exp + grace);
                if let Some(d) = tr.disc_at_us {
                    cand(d + intv);
                }
            }
            if let Some(c) = &self.coord {
                let last = self
                    .tracks
                    .get(&c.holder)
                    .map_or(0, |t| t.last_arrival_us)
                    .max(c.adopted_us);
                cand(last + live);
            }
        }
        best
    }

    /// Coalesced updates for one subscription, at most `max`.
    pub fn drain(&mut self, client: ClientId, sub: u32, max: usize) -> Vec<Update> {
        self.subs.drain(client, sub, max)
    }

    pub fn persist(&self) -> Persisted {
        self.dirty.set(false);
        match &self.grants {
            None => Persisted {
                room: self.room,
                ..Default::default()
            },
            Some(g) => Persisted {
                room: self.room,
                records: g.records().map(|r| r.bytes.clone()).collect(),
                state: self
                    .store
                    .persistent_records()
                    .into_iter()
                    .map(|r| r.bytes.clone())
                    .collect(),
            },
        }
    }

    pub fn status(&self, now: Now) -> Status {
        let Some(g) = &self.grants else {
            return Status {
                room: None,
                me: self.my_key,
                lifecycle: Lifecycle::Dead,
                coordinator: None,
                members: Vec::new(),
                records: 0,
                durable_bytes_room: 0,
                durable_bytes_mine: 0,
            };
        };
        let pv = self.presence_views(now.mono_us);
        let members = g
            .authority()
            .members
            .keys()
            .map(|k| MemberInfo {
                key: *k,
                keyholder: g.is_keyholder(k),
                inviter: g.is_inviter(k),
                connected: self.established(k).is_some(),
                presence: self
                    .store
                    .member_best(*k, PRESENCE, Class::Durable)
                    .and_then(|r| Presence::decode(&r.value).ok().flatten()),
                fresh: pv.get(k).is_some_and(|p| p.online && p.fresh),
            })
            .collect();
        Status {
            room: Some(self.room),
            me: self.my_key,
            lifecycle: g.lifecycle(),
            coordinator: g.coordinator().map(|(e, c)| (e, c.holder)),
            members,
            records: g.len(),
            durable_bytes_room: self.store.usage(Scope::Room, Key::ZERO).bytes,
            durable_bytes_mine: self.store.usage(Scope::Member, self.my_key).bytes,
        }
    }

    /// `None` while joining (before the genesis arrived).
    pub fn grants(&self) -> Option<&GrantSet> {
        self.grants.as_ref()
    }
    pub fn room_id(&self) -> RoomId {
        self.room
    }
    pub fn key(&self) -> Key {
        self.my_key
    }

    // Additions for the daemon, simulator and tests.

    /// Everything currently visible to subscribers, by path.
    pub fn visible_state(&self) -> &BTreeMap<String, Vis> {
        &self.published
    }
    pub fn stats(&self) -> &Stats {
        &self.stats
    }
    /// True once this node was removed from the room (revoked, or retired itself).
    pub fn is_removed(&self) -> bool {
        self.removed
    }
    /// Peers with an established connection.
    pub fn connected_peers(&self) -> Vec<Key> {
        self.by_peer
            .iter()
            .filter(|(_, c)| self.conns[c].state == CState::Established)
            .map(|(k, _)| *k)
            .collect()
    }
    pub fn coordinator(&self) -> Option<(Epoch, Key)> {
        let g = self.grants.as_ref()?;
        g.coordinator().map(|(e, c)| (e, c.holder))
    }
    pub fn member_incarnation(&self) -> u32 {
        self.cfg.member_incarnation
    }

    // ------------------------------------------------------------------
    // Small helpers
    // ------------------------------------------------------------------

    fn emit(&mut self, event: LocalEvent) {
        for c in &self.clients {
            self.out.push_back(Output::Local {
                client: *c,
                event: event.clone(),
            });
        }
    }

    fn emit_to(&mut self, client: ClientId, event: LocalEvent) {
        self.out.push_back(Output::Local { client, event });
    }

    fn reply(&mut self, client: ClientId, req: ReqId, reply: Reply) {
        self.emit_to(client, LocalEvent::Reply { req, reply });
    }

    fn err(&mut self, client: ClientId, req: ReqId, code: ErrorCode) {
        self.reply(client, req, Reply::Error(code));
    }

    fn send(&mut self, conn: ConnId, lane: Lane, f: Frame) {
        self.out.push_back(Output::Send {
            conn,
            lane,
            bytes: f.encode(),
        });
    }

    fn mark_persist(&mut self) {
        if !self.dirty.replace(true) {
            self.out.push_back(Output::PersistDirty);
        }
    }

    /// The next member-scope sequence number, `None` once the counter is exhausted
    /// (a peer can push it by replaying a record of ours near `u64::MAX`).
    fn bump_seq(&mut self) -> Option<u64> {
        self.my_seq = self.my_seq.checked_add(1)?;
        Some(self.my_seq)
    }

    fn nonce16(&mut self) -> [u8; 16] {
        self.entropy.bytes()
    }

    fn established(&self, k: &Key) -> Option<ConnId> {
        self.by_peer
            .get(k)
            .copied()
            .filter(|c| self.conns[c].state == CState::Established)
    }

    fn established_conns(&self) -> Vec<(ConnId, Key)> {
        self.conns
            .iter()
            .filter(|(_, c)| c.state == CState::Established)
            .map(|(id, c)| (*id, c.peer))
            .collect()
    }

    fn open_generation(&self) -> Option<u32> {
        match self.grants.as_ref()?.lifecycle() {
            Lifecycle::Open(g) => Some(g),
            _ => None,
        }
    }

    fn is_member_open(&self) -> bool {
        self.grants.as_ref().is_some_and(|g| {
            g.is_member(&self.my_key) && matches!(g.lifecycle(), Lifecycle::Open(_))
        }) && !self.removed
            && !self.left
    }

    fn dialing_active(&self) -> bool {
        if self.removed || self.left {
            return false;
        }
        match &self.grants {
            None => self.joining.is_some(),
            Some(_) => self.open_generation().is_some(),
        }
    }

    fn presence_active(&self) -> bool {
        self.is_member_open()
    }

    // ------------------------------------------------------------------
    // Connections and handshake
    // ------------------------------------------------------------------

    fn drop_conn(&mut self, conn: ConnId) -> Option<Conn> {
        let c = self.conns.remove(&conn)?;
        if self.by_peer.get(&c.peer) == Some(&conn) {
            self.by_peer.remove(&c.peer);
        }
        let lost: Vec<((ConnId, u64), (ClientId, Key))> = self
            .pending_msgs
            .range((conn, 0)..=(conn, u64::MAX))
            .map(|(k, v)| (*k, *v))
            .collect();
        for (k, (client, to)) in lost {
            self.pending_msgs.remove(&k);
            self.emit_to(
                client,
                LocalEvent::MessageResult {
                    msg_id: k.1,
                    to,
                    status: MessageStatus::Lost,
                },
            );
        }
        if c.state == CState::Established {
            if let Some(t) = self.tracks.get_mut(&c.peer) {
                if !self.by_peer.contains_key(&c.peer) {
                    t.disc_at_us = Some(self.now.mono_us);
                }
            }
            if !self.conns.values().any(|c| c.state == CState::Established) {
                self.synced = false;
            }
        }
        Some(c)
    }

    fn close_conn(&mut self, conn: ConnId, reason: Reason) {
        if self.drop_conn(conn).is_some() {
            self.out.push_back(Output::Close { conn, reason });
        }
    }

    fn close_all(&mut self, reason: Reason) {
        let ids: Vec<ConnId> = self.conns.keys().copied().collect();
        for id in ids {
            self.close_conn(id, reason);
        }
    }

    fn reject(&mut self, conn: ConnId, reason: Reason) {
        self.send(
            conn,
            Lane::Control,
            Frame::Reject {
                reason,
                detail: Vec::new(),
            },
        );
        self.close_conn(conn, reason);
    }

    fn my_hello(&self) -> Frame {
        Frame::Hello(Hello {
            version: PROTOCOL_VERSION,
            caps: self.cfg.caps,
            room: self.room,
            member: self.my_key,
            daemon_incarnation: self.cfg.daemon_incarnation,
            open_generation: self.open_generation().unwrap_or(0),
            proof: self
                .joining
                .as_ref()
                .map(|j| j.proof.iter().map(|r| r.bytes.clone()).collect())
                .unwrap_or_default(),
        })
    }

    fn on_connected(&mut self, conn: ConnId, peer: Key, outbound: bool) {
        if outbound {
            if let Some(d) = self.dial.get_mut(&peer) {
                d.in_flight_until = None;
            }
        }
        let live = !self.removed && !self.left && (self.grants.is_some() || self.joining.is_some());
        if !live || peer == self.my_key {
            self.out.push_back(Output::Close {
                conn,
                reason: Reason::Shutdown,
            });
            return;
        }
        if let Some(&old) = self.by_peer.get(&peer) {
            let old_dialler = if self.conns[&old].outbound {
                self.my_key
            } else {
                peer
            };
            let new_dialler = if outbound { self.my_key } else { peer };
            if new_dialler > old_dialler {
                self.out.push_back(Output::Close {
                    conn,
                    reason: Reason::Duplicate,
                });
                return;
            }
            self.close_conn(old, Reason::Duplicate);
        }
        self.conns.insert(
            conn,
            Conn {
                peer,
                outbound,
                state: CState::Handshake,
                deadline_us: self.now.mono_us + self.cfg.timing.hello_timeout_ms * 1000,
                sync: None,
                early: Vec::new(),
                early_bytes: 0,
            },
        );
        self.by_peer.insert(peer, conn);
        if outbound {
            let hello = self.my_hello();
            self.send(conn, Lane::Control, hello);
        }
    }

    fn on_hello(&mut self, conn: ConnId, hello: Hello) {
        let (peer, outbound, state) = {
            let c = &self.conns[&conn];
            (c.peer, c.outbound, c.state)
        };
        if state != CState::Handshake {
            return self.close_conn(conn, Reason::Shutdown);
        }
        if hello.version != PROTOCOL_VERSION {
            return self.reject(conn, Reason::Version);
        }
        if hello.room != self.room {
            return self.reject(conn, Reason::WrongRoom);
        }
        if hello.member != peer {
            return self.reject(conn, Reason::NotMember);
        }
        let verdict = if outbound {
            self.check_acceptor(peer).map(|s| (s, Vec::new()))
        } else {
            self.check_dialler(peer, &hello)
        };
        let (new_state, adopted) = match verdict {
            Ok(v) => v,
            Err(reason) => return self.reject(conn, reason),
        };
        if !outbound {
            let hello = self.my_hello();
            self.send(conn, Lane::Control, hello);
        }
        if let Some(c) = self.conns.get_mut(&conn) {
            c.state = new_state;
            c.sync = Some(Vec::new());
        }
        if new_state == CState::Established {
            self.on_established(peer);
        }
        if !adopted.is_empty() {
            // Redemption: flood the two records to everyone else.
            self.after_grants_change(adopted, Some(conn));
        }
        if new_state == CState::Established && self.conns.contains_key(&conn) {
            self.send_sync(conn);
            self.replay_early(conn);
        }
    }

    fn on_established(&mut self, peer: Key) {
        if let Some(t) = self.tracks.get_mut(&peer) {
            t.disc_at_us = None;
        }
        if let Some(d) = self.dial.get_mut(&peer) {
            d.in_flight_until = None;
        }
    }

    /// We dialled `peer` and it answered with a hello.
    fn check_acceptor(&self, peer: Key) -> Result<CState, Reason> {
        match &self.grants {
            Some(g) => g
                .admit(&peer, self.now.unix_ms)
                .map(|_| CState::Established)
                .map_err(admit_reason),
            None => {
                let j = self.joining.as_ref().ok_or(Reason::NotMember)?;
                if j.token.peers.contains(&peer) {
                    Ok(CState::Established)
                } else {
                    Err(Reason::NotMember)
                }
            }
        }
    }

    /// `peer` dialled us. Returns the connection's next state and any records adopted
    /// from a redemption proof.
    fn check_dialler(&mut self, peer: Key, hello: &Hello) -> Result<(CState, Vec<Record>), Reason> {
        let unix = self.now.unix_ms;
        let Some(g) = &self.grants else {
            return Err(Reason::NotMember);
        };
        if g.is_member(&peer) {
            match g.admit(&peer, unix) {
                Ok(()) => return Ok((CState::Established, Vec::new())),
                Err(AdmitError::Closed) => {
                    return match g.lifecycle() {
                        Lifecycle::Closed(cg) if hello.open_generation > cg => {
                            Ok((CState::Probation, Vec::new()))
                        }
                        _ => Err(Reason::Closed),
                    }
                }
                // An expired member may come back with a fresh invite proof.
                Err(AdmitError::Expired) if hello.proof.len() == 2 => {}
                Err(e) => return Err(admit_reason(e)),
            }
        }
        if hello.proof.len() != 2 {
            return Err(Reason::NotMember);
        }
        let invite = Record::decode(&hello.proof[0]).map_err(|_| Reason::InviteInvalid)?;
        let member = Record::decode(&hello.proof[1]).map_err(|_| Reason::InviteInvalid)?;
        let next = g
            .check_redemption(&invite, &member, peer, unix)
            .map_err(admit_reason)?;
        self.grants = Some(next);
        Ok((CState::Established, vec![invite, member]))
    }

    /// Our records and durable state snapshot, for a newly established connection.
    fn send_sync(&mut self, conn: ConnId) {
        let Some(g) = &self.grants else { return };
        let recs: Vec<Vec<u8>> = g.records().map(|r| r.bytes.clone()).collect();
        for chunk in recs.chunks(RECORDS_PER_FRAME) {
            self.send(conn, Lane::Control, Frame::Records(chunk.to_vec()));
        }
        // The receiver judges membership only once it has the whole set.
        self.send(conn, Lane::Control, Frame::RecordsEnd);
        let snap: Vec<Vec<u8>> = self
            .store
            .durable_records()
            .into_iter()
            .map(|r| r.bytes.clone())
            .collect();
        self.send(
            conn,
            Lane::State,
            Frame::SnapshotBegin {
                count: snap.len() as u32,
            },
        );
        for s in snap {
            self.send(conn, Lane::State, Frame::State(s));
        }
        self.send(conn, Lane::State, Frame::SnapshotEnd);
    }

    fn on_frame(&mut self, conn: ConnId, lane: Lane, bytes: Vec<u8>) {
        let Some(c) = self.conns.get(&conn) else {
            return;
        };
        let state = c.state;
        let frame = match Frame::decode(&bytes) {
            Ok(f) => f,
            Err(_) => {
                self.stats.bad_frames += 1;
                return;
            }
        };
        match frame {
            Frame::Hello(hello) => self.on_hello(conn, hello),
            Frame::Reject { .. } => {
                self.close_conn(conn, Reason::Shutdown);
            }
            Frame::Goodbye { .. } => {
                self.drop_conn(conn);
            }
            Frame::Records(recs) => match state {
                CState::Handshake => self.close_conn(conn, Reason::Shutdown),
                _ => {
                    if !self.buffer_sync_records(conn, &recs) {
                        self.on_records(conn, recs);
                        self.promote_probation(conn);
                    }
                }
            },
            Frame::RecordsEnd => match state {
                CState::Handshake => self.close_conn(conn, Reason::Shutdown),
                _ => {
                    let buffered = self.conns.get_mut(&conn).and_then(|c| c.sync.take());
                    if let Some(buf) = buffered {
                        self.apply_records(conn, buf);
                        self.promote_probation(conn);
                    }
                }
            },
            Frame::SnapshotBegin { .. }
            | Frame::State(_)
            | Frame::SnapshotEnd
            | Frame::Message { .. }
            | Frame::MessageAck { .. }
                if state != CState::Established =>
            {
                self.hold_early(conn, lane, bytes);
            }
            _ if state != CState::Established => {}
            Frame::SnapshotBegin { .. } => {}
            Frame::State(b) => match StateRecord::decode(self.room, &b) {
                Ok(rec) if rec.class == Class::Durable => self.ingest_state(rec, Some(conn)),
                _ => self.stats.bad_state += 1,
            },
            Frame::SnapshotEnd => {
                if self.grants.is_some() {
                    self.finish_snapshot();
                } else {
                    // The state lane overtook the records lane: finish once adopted.
                    self.snapshot_end_early = true;
                }
            }
            Frame::Message {
                msg_id,
                channel,
                flags,
                payload,
                ..
            } => self.on_message(conn, msg_id, channel, flags, payload),
            Frame::MessageAck { msg_id, status } => {
                if let Some((client, to)) = self.pending_msgs.remove(&(conn, msg_id)) {
                    self.emit_to(
                        client,
                        LocalEvent::MessageResult {
                            msg_id,
                            to,
                            status: MessageStatus::Acked(status),
                        },
                    );
                }
            }
        }
    }

    fn finish_snapshot(&mut self) {
        if !self.synced && self.grants.is_some() {
            self.synced = true;
            self.refresh();
            let edges = self.subs.reset_all(&self.published);
            self.ready(edges);
        }
    }

    fn hold_early(&mut self, conn: ConnId, lane: Lane, bytes: Vec<u8>) {
        let Some(c) = self.conns.get_mut(&conn) else {
            return;
        };
        if c.early.len() >= EARLY_FRAMES || c.early_bytes + bytes.len() > EARLY_BYTES {
            self.stats.bad_frames += 1;
            return;
        }
        c.early_bytes += bytes.len();
        c.early.push((lane, bytes));
    }

    /// Process what overtook the hello, now that `conn` is established.
    fn replay_early(&mut self, conn: ConnId) {
        let early = match self.conns.get_mut(&conn) {
            Some(c) => {
                c.early_bytes = 0;
                std::mem::take(&mut c.early)
            }
            None => return,
        };
        for (lane, bytes) in early {
            self.on_frame(conn, lane, bytes);
        }
    }

    /// A closed node that learned of a reopen from the peer's records: finish the
    /// handshake.
    fn promote_probation(&mut self, conn: ConnId) {
        let Some(c) = self.conns.get(&conn) else {
            return;
        };
        if c.state != CState::Probation {
            return;
        }
        let peer = c.peer;
        let ok = self
            .grants
            .as_ref()
            .is_some_and(|g| g.admit(&peer, self.now.unix_ms).is_ok());
        if ok {
            self.conns.get_mut(&conn).unwrap().state = CState::Established;
            self.on_established(peer);
            self.send_sync(conn);
            self.replay_early(conn);
        }
    }

    fn ready(&mut self, edges: Vec<(ClientId, u32)>) {
        for (client, sub) in edges {
            self.emit_to(client, LocalEvent::Ready { sub });
        }
    }

    // ------------------------------------------------------------------
    // Records (grants, lifecycle, claims)
    // ------------------------------------------------------------------

    fn decode_records(&mut self, recs: &[Vec<u8>]) -> Vec<Record> {
        let mut decoded = Vec::new();
        for b in recs {
            match Record::decode(b) {
                Ok(r) => decoded.push(r),
                Err(_) => self.stats.bad_records += 1,
            }
        }
        decoded
    }

    /// While `conn` is in its initial sync, hold `recs` until `RecordsEnd` and return
    /// true. Bounded: a peer can't hold more than a full grant set (a bigger sync
    /// closes the connection).
    fn buffer_sync_records(&mut self, conn: ConnId, recs: &[Vec<u8>]) -> bool {
        let syncing = self.conns.get(&conn).is_some_and(|c| c.sync.is_some());
        if !syncing {
            return false;
        }
        let decoded = self.decode_records(recs);
        let Some(buf) = self.conns.get_mut(&conn).and_then(|c| c.sync.as_mut()) else {
            return true;
        };
        // +1: the genesis rides in the set but not in `MAX_RECORDS`.
        if buf.len() + decoded.len() > crate::grants::MAX_RECORDS + 1 {
            self.stats.rejected_records += 1;
            self.close_conn(conn, Reason::Shutdown);
            return true;
        }
        buf.extend(decoded);
        true
    }

    /// An incremental `records` frame on an established connection.
    fn on_records(&mut self, conn: ConnId, recs: Vec<Vec<u8>>) {
        let decoded = self.decode_records(&recs);
        self.apply_records(conn, decoded);
    }

    fn apply_records(&mut self, conn: ConnId, decoded: Vec<Record>) {
        if self.grants.is_none() {
            return self.adopt_genesis(conn, decoded);
        }
        let g = self.grants.as_mut().unwrap();
        let res = g.insert_all(decoded.clone());
        let mut new = Vec::new();
        for (r, res) in decoded.into_iter().zip(res) {
            match res {
                Ok(true) => new.push(r),
                Ok(false) => {}
                Err(_) => self.stats.rejected_records += 1,
            }
        }
        if !new.is_empty() {
            self.after_grants_change(new, Some(conn));
        }
    }

    /// A joiner's first `records` frame: build the grant set from the genesis.
    fn adopt_genesis(&mut self, conn: ConnId, recs: Vec<Record>) {
        let Some(genesis) = recs.iter().find(|r| r.kind == Kind::Genesis) else {
            return;
        };
        if genesis.room != self.room {
            self.stats.rejected_records += 1;
            return self.close_conn(conn, Reason::WrongRoom);
        }
        let mut g = GrantSet::new(genesis.clone());
        let rest: Vec<Record> = recs
            .into_iter()
            .filter(|r| r.kind != Kind::Genesis)
            .collect();
        for r in g.insert_all(rest) {
            if r.is_err() {
                self.stats.rejected_records += 1;
            }
        }
        if !g.is_member(&self.my_key) {
            // The peer that sent this did not admit us.
            return self.close_conn(conn, Reason::NotMember);
        }
        self.grants = Some(g);
        self.joining = None;
        self.adopt_initial_view();
        // Everyone is newly known (Joined events), and the open is news (RoomOpened,
        // Coordinator::Changed).
        self.roster_known.clear();
        self.lifecycle_seen = Lifecycle::Dead;
        self.coord = None;
        self.after_grants_change(Vec::new(), Some(conn));
        for (c, _) in self.established_conns() {
            self.send_sync(c);
        }
        if std::mem::take(&mut self.snapshot_end_early) {
            self.finish_snapshot();
        }
    }

    /// Flood `new` records to every established peer but `source`, then reconcile.
    fn after_grants_change(&mut self, new: Vec<Record>, source: Option<ConnId>) {
        let bytes: Vec<Vec<u8>> = new.iter().map(|r| r.bytes.clone()).collect();
        if self.grants.is_some() && !bytes.is_empty() {
            for (c, _) in self.established_conns() {
                if Some(c) == source {
                    continue;
                }
                for chunk in bytes.chunks(RECORDS_PER_FRAME) {
                    self.send(c, Lane::Control, Frame::Records(chunk.to_vec()));
                }
            }
        }
        self.reconcile();
    }

    /// Insert records we issued ourselves, flood them and reconcile.
    fn commit_local_records(&mut self, recs: Vec<Record>) -> Result<(), ErrorCode> {
        let Some(g) = self.grants.as_mut() else {
            return Err(ErrorCode::NoRoom);
        };
        let res = g.insert_all(recs.clone());
        let mut full = false;
        let mut new = Vec::new();
        for (r, res) in recs.into_iter().zip(res) {
            match res {
                Ok(true) => new.push(r),
                Ok(false) => {}
                Err(InsertError::Full) => full = true,
                Err(_) => self.stats.rejected_records += 1,
            }
        }
        if full {
            self.stats.rejected_records += 1;
        }
        if !new.is_empty() {
            self.after_grants_change(new, None);
        }
        if full {
            Err(ErrorCode::OverBudget)
        } else {
            Ok(())
        }
    }

    /// The grant set with `rec` added, to see whether it would count before it is
    /// issued.
    fn probe(&self, rec: &Record) -> Result<GrantSet, ErrorCode> {
        let mut p = self.grants.as_ref().ok_or(ErrorCode::NoRoom)?.clone();
        match p.insert(rec.clone()) {
            Ok(_) => Ok(p),
            Err(InsertError::Full) => Err(ErrorCode::OverBudget),
            Err(_) => Err(ErrorCode::Unknown),
        }
    }

    /// Re-derive everything that depends on the grant set.
    fn reconcile(&mut self) {
        let Some(g) = self.grants.as_ref() else {
            return;
        };
        let me = self.my_key;
        if !g.is_member_at(&me, self.now.unix_ms) {
            return self.on_removed();
        }
        if self.removed {
            self.on_readmitted();
        }
        let g = self.grants.as_ref().unwrap();
        let life = g.lifecycle();
        let roster: BTreeSet<Key> = g
            .authority()
            .members
            .keys()
            .copied()
            .filter(|k| *k != me)
            .collect();
        self.mark_persist();
        let joined: Vec<Key> = roster.difference(&self.roster_known).copied().collect();
        let gone: Vec<Key> = self.roster_known.difference(&roster).copied().collect();
        self.roster_known = roster;
        for k in joined {
            self.dial.entry(k).or_default();
            self.emit(LocalEvent::Member {
                key: k,
                event: MemberEvent::Joined,
            });
        }
        for k in gone {
            self.store.purge_writer(k);
            self.orphans.retain(|r| r.writer != k);
            self.tracks.remove(&k);
            self.pstatus.remove(&k);
            self.dial.remove(&k);
            self.emit(LocalEvent::Member {
                key: k,
                event: MemberEvent::Removed,
            });
        }
        // Lifecycle transitions.
        let prev = self.lifecycle_seen;
        if life != prev {
            self.lifecycle_seen = life;
            match life {
                Lifecycle::Closed(generation) => {
                    self.emit(LocalEvent::RoomClosed { generation });
                    self.close_all(Reason::Closed);
                    self.coord = None;
                }
                Lifecycle::Open(generation) => {
                    self.emit(LocalEvent::RoomOpened { generation });
                    self.next_presence_us = 0;
                    let now = self.now.mono_us;
                    for d in self.dial.values_mut() {
                        d.next_us = now;
                        d.in_flight_until = None;
                    }
                    self.store.prune_stale_generations(generation);
                }
                Lifecycle::Dead => {}
            }
        }
        // Re-admission check on every established connection.
        let g = self.grants.as_ref().unwrap();
        let unix = self.now.unix_ms;
        let bad: Vec<ConnId> = self
            .conns
            .iter()
            .filter(|(_, c)| c.state == CState::Established)
            .filter(|(_, c)| {
                matches!(
                    g.admit(&c.peer, unix),
                    Err(AdmitError::NotMember | AdmitError::Expired)
                )
            })
            .map(|(id, _)| *id)
            .collect();
        for id in bad {
            self.close_conn(id, Reason::Revoked);
        }
        // Epoch holders may have changed; writers may have become members.
        self.store.revalidate_room(self.grants.as_ref().unwrap());
        let retry: Vec<StateRecord> = self.orphans.drain(..).collect();
        for r in retry {
            self.ingest_state(r, None);
        }
        self.update_coordinator();
        self.refresh();
    }

    /// The grant set (re)admits this node after it was marked removed: start again as
    /// if freshly joined (everyone is newly known, the open is news).
    fn on_readmitted(&mut self) {
        self.removed = false;
        self.roster_known.clear();
        self.lifecycle_seen = Lifecycle::Dead;
        self.coord = None;
        self.next_presence_us = 0;
    }

    fn on_removed(&mut self) {
        if self.removed {
            return;
        }
        self.removed = true;
        self.emit(LocalEvent::Removed);
        self.close_all(Reason::Revoked);
        self.dial.clear();
        self.coord = None;
        self.mark_persist();
    }

    fn update_coordinator(&mut self) {
        let cur = match (&self.grants, self.lifecycle_seen) {
            (Some(g), Lifecycle::Open(_)) => g.coordinator().map(|(e, c)| (e, c.holder)),
            _ => None,
        };
        match (&self.coord, cur) {
            (_, None) => self.coord = None,
            (Some(c), Some((e, h))) if c.epoch == e && c.holder == h => {}
            (_, Some((epoch, holder))) => {
                self.coord = Some(CoordState {
                    epoch,
                    holder,
                    adopted_us: self.now.mono_us,
                    status: CoordStatus::Fresh,
                });
                self.emit(LocalEvent::Coordinator(CoordinatorEvent::Changed {
                    epoch,
                    holder,
                }));
            }
        }
    }

    // ------------------------------------------------------------------
    // State records
    // ------------------------------------------------------------------

    fn ingest_state(&mut self, rec: StateRecord, from: Option<ConnId>) {
        if self.grants.is_none() {
            // A joiner's state lane can overtake its records lane: hold what arrives
            // (bounded) and ingest it once the grant set exists.
            if rec.class == Class::Durable
                && self.orphans.len() < PRE_GRANT_CAP
                && !self.orphans.iter().any(|o| o.bytes == rec.bytes)
            {
                self.orphans.push_back(rec);
            }
            return;
        }
        if rec.writer == self.my_key && rec.scope == Scope::Member && rec.class == Class::Durable {
            // Yjs-style re-assert: nobody else may write our keys.
            if let Some(cur) = self.store.best(&Slot::of(&rec)).cloned() {
                if rec.version > cur.version
                    || (rec.version == cur.version && rec.bytes != cur.bytes)
                {
                    self.reassert(&rec, cur);
                }
                return;
            }
        }
        let res = self
            .store
            .insert(rec.clone(), self.grants.as_ref().unwrap());
        match res {
            Insert::Improved => {
                if is_presence(&rec) && rec.writer != self.my_key {
                    let t = self.tracks.entry(rec.writer).or_default();
                    t.last_arrival_us = self.now.mono_us;
                    t.disc_at_us = None;
                }
                if rec.class == Class::Durable {
                    self.flood_state(&rec, from);
                    if rec.lifetime == Lifetime::Persistent {
                        self.mark_persist();
                    }
                }
            }
            Insert::Orphan => {
                if !self.orphans.iter().any(|o| o.bytes == rec.bytes) {
                    if self.orphans.len() >= ORPHAN_CAP {
                        self.orphans.pop_front();
                        self.stats.orphans_dropped += 1;
                    }
                    self.orphans.push_back(rec);
                }
            }
            Insert::Stale => {}
            Insert::Rejected(why) => {
                if why == "over budget" {
                    self.stats.over_cap_state += 1;
                } else {
                    self.stats.rejected_state += 1;
                }
            }
        }
    }

    /// Someone sent us our own key at a version at least ours. Bump past it and
    /// publish our current value again.
    fn reassert(&mut self, received: &StateRecord, current: StateRecord) {
        let Some(generation) = self.open_generation() else {
            return;
        };
        let inc = self.cfg.member_incarnation;
        let mine = Epoch {
            generation,
            term: inc,
        };
        if received.version.epoch() > mine {
            // Can't outrank a future incarnation's record from here.
            self.stats.rejected_state += 1;
            return;
        }
        self.my_seq = self.my_seq.max(received.version.seq);
        if is_presence(&current) {
            if !self.left {
                self.write_presence(!current.value.is_empty());
            }
            return;
        }
        let Some(seq) = self.bump_seq() else {
            return;
        };
        let version = Version {
            generation,
            term: inc,
            seq,
        };
        if let Ok(rec) = StateRecord::build(
            &self.me,
            self.room,
            current.class,
            current.lifetime,
            Scope::Member,
            version,
            &current.name,
            &current.value,
        ) {
            self.commit_own_state(rec);
        }
    }

    fn flood_state(&mut self, rec: &StateRecord, except: Option<ConnId>) {
        let lane = if is_presence(rec) {
            Lane::Control
        } else {
            Lane::State
        };
        for (c, _) in self.established_conns() {
            if Some(c) != except {
                self.send(c, lane, Frame::State(rec.bytes.clone()));
            }
        }
    }

    /// Store and publish a record this node wrote.
    fn commit_own_state(&mut self, rec: StateRecord) -> Insert {
        let r = self
            .store
            .insert(rec.clone(), self.grants.as_ref().unwrap());
        if r == Insert::Improved {
            match rec.class {
                Class::Durable => {
                    self.flood_state(&rec, None);
                    if rec.lifetime == Lifetime::Persistent {
                        self.mark_persist();
                    }
                }
                Class::Live => {
                    let bytes = Datagram::LiveState(rec.bytes.clone()).encode();
                    for (c, _) in self.established_conns() {
                        self.out.push_back(Output::SendDatagram {
                            conn: c,
                            bytes: bytes.clone(),
                            max_age_ms: 200,
                        });
                    }
                }
            }
        }
        r
    }

    fn write_presence(&mut self, online: bool) {
        let Some(generation) = self.open_generation() else {
            return;
        };
        if !self
            .grants
            .as_ref()
            .is_some_and(|g| g.is_member(&self.my_key))
        {
            return;
        }
        let since = match self.my_since {
            Some((g, ms)) if g == generation => ms,
            _ => {
                self.my_since = Some((generation, self.now.unix_ms));
                self.now.unix_ms
            }
        };
        let value = if online {
            Presence {
                since_ms: since,
                daemon_incarnation: self.cfg.daemon_incarnation,
            }
            .encode()
        } else {
            Vec::new()
        };
        let Some(seq) = self.bump_seq() else {
            return;
        };
        let version = Version {
            generation,
            term: self.cfg.member_incarnation,
            seq,
        };
        if let Ok(rec) = StateRecord::build(
            &self.me,
            self.room,
            Class::Durable,
            Lifetime::PresenceBound,
            Scope::Member,
            version,
            PRESENCE,
            &value,
        ) {
            self.commit_own_state(rec);
        }
        self.next_presence_us = self.now.mono_us + self.cfg.timing.presence_interval_ms * 1000;
    }

    fn on_datagram(&mut self, conn: ConnId, bytes: Vec<u8>) {
        let Some(c) = self.conns.get(&conn) else {
            return;
        };
        if c.state != CState::Established {
            return;
        }
        let peer = c.peer;
        match Datagram::decode(&bytes) {
            Ok(Datagram::LiveState(b)) => match StateRecord::decode(self.room, &b) {
                Ok(rec) if rec.class == Class::Live && rec.writer == peer => {
                    // Live records come straight from the writer's connection and are
                    // never forwarded.
                    if let Some(g) = self.grants.as_ref() {
                        if let Insert::Rejected(_) = self.store.insert(rec, g) {
                            self.stats.rejected_state += 1;
                        }
                    }
                }
                _ => self.stats.bad_state += 1,
            },
            Ok(Datagram::App {
                channel,
                send_ms,
                payload,
                ..
            }) => {
                let is_member = self.grants.as_ref().is_some_and(|g| g.is_member(&peer));
                let Some(targets) = self.channels.get(&channel).filter(|_| is_member) else {
                    self.stats.dropped_datagrams += 1;
                    return;
                };
                let targets: Vec<ClientId> = targets.iter().copied().collect();
                let arrival_ms = (self.now.mono_us / 1000) as u32;
                let delta = arrival_ms.wrapping_sub(send_ms);
                let base = self.dgram_base.entry(peer).or_insert(delta);
                let rel = delta.wrapping_sub(*base) as i32;
                let age = if rel < 0 {
                    *base = delta;
                    0
                } else {
                    rel as u32
                };
                for client in targets {
                    self.emit_to(
                        client,
                        LocalEvent::Datagram {
                            from: peer,
                            channel,
                            payload: payload.clone(),
                            age_ms: age,
                        },
                    );
                }
            }
            // Voice and visemes take the driver's realtime fast path and never reach the
            // core; one that does (a driver without a voice lane) is dropped.
            Ok(Datagram::Voice(_)) | Ok(Datagram::Viseme(_)) => self.stats.dropped_datagrams += 1,
            Err(_) => self.stats.bad_frames += 1,
        }
    }

    // ------------------------------------------------------------------
    // Presence, liveness, coordinator
    // ------------------------------------------------------------------

    fn presence_views(&self, now_us: u64) -> BTreeMap<Key, PresView> {
        let mut out = BTreeMap::new();
        let Some(g) = &self.grants else {
            return out;
        };
        let t = &self.cfg.timing;
        let exp = t.presence_expiry_ms * 1000;
        let grace = t.rejoin_grace_ms * 1000;
        let intv = t.presence_interval_ms * 1000;
        for k in g.authority().members.keys() {
            let Some(rec) = self.store.member_best(*k, PRESENCE, Class::Durable) else {
                continue;
            };
            let p = Presence::decode(&rec.value).ok().flatten();
            let (live, fresh) = if *k == self.my_key {
                (true, true)
            } else if let Some(tr) = self.tracks.get(k) {
                let age = now_us.saturating_sub(tr.last_arrival_us);
                let fast = tr
                    .disc_at_us
                    .is_some_and(|d| tr.last_arrival_us <= d && now_us >= d + intv);
                (age < exp + grace, age < exp && !fast)
            } else {
                (false, false)
            };
            out.insert(
                *k,
                PresView {
                    epoch: rec.version.epoch(),
                    online: p.is_some(),
                    since_ms: p.map_or(0, |p| p.since_ms),
                    live,
                    fresh,
                },
            );
        }
        out
    }

    /// Member presence events and coordinator liveness / claims.
    fn liveness(&mut self) {
        if self.grants.is_none() || self.removed || self.left {
            return;
        }
        let now = self.now.mono_us;
        let pv = self.presence_views(now);
        let open = self.open_generation();
        // Member events.
        let roster: Vec<Key> = self.roster_known.iter().copied().collect();
        for k in roster {
            let st = match (pv.get(&k), open) {
                (Some(p), Some(g)) if p.epoch.generation == g => {
                    if !p.online {
                        PStatus::Offline
                    } else if p.fresh {
                        PStatus::Online
                    } else {
                        PStatus::Unresponsive
                    }
                }
                _ => PStatus::Unknown,
            };
            let prev = self.pstatus.insert(k, st).unwrap_or(PStatus::Unknown);
            if prev == st {
                continue;
            }
            let ev = match (prev, st) {
                (_, PStatus::Online) => Some(MemberEvent::Online),
                (PStatus::Online | PStatus::Unresponsive, PStatus::Offline) => {
                    Some(MemberEvent::Offline)
                }
                (PStatus::Online, PStatus::Unresponsive) => Some(MemberEvent::Unresponsive),
                _ => None,
            };
            if let Some(event) = ev {
                self.emit(LocalEvent::Member { key: k, event });
            }
        }
        // Coordinator liveness.
        let live_us = self.cfg.timing.coordinator_liveness_ms * 1000;
        let Some(c) = &self.coord else {
            return;
        };
        let (epoch, holder, adopted, prev) = (c.epoch, c.holder, c.adopted_us, c.status);
        let status = if holder == self.my_key {
            CoordStatus::Fresh
        } else if pv
            .get(&holder)
            .is_some_and(|p| p.epoch.generation == epoch.generation && !p.online)
        {
            CoordStatus::Left
        } else {
            let last = self
                .tracks
                .get(&holder)
                .map_or(0, |t| t.last_arrival_us)
                .max(adopted);
            if now.saturating_sub(last) >= live_us {
                CoordStatus::Unresponsive
            } else {
                CoordStatus::Fresh
            }
        };
        self.coord.as_mut().unwrap().status = status;
        if status != prev {
            match status {
                CoordStatus::Left => self.emit(LocalEvent::Coordinator(CoordinatorEvent::Left {
                    epoch,
                    holder,
                })),
                CoordStatus::Unresponsive => {
                    self.emit(LocalEvent::Coordinator(CoordinatorEvent::Unresponsive {
                        epoch,
                        holder,
                    }))
                }
                CoordStatus::Fresh => {}
            }
        }
        if status == CoordStatus::Fresh {
            return;
        }
        // Candidate: the present, fresh member with the lowest (since_ms, key).
        let Some(g) = open else { return };
        let mut best: Option<(u64, Key)> = None;
        for k in self.grants.as_ref().unwrap().authority().members.keys() {
            if *k == holder {
                continue;
            }
            let Some(p) = pv.get(k) else { continue };
            if !p.online || !p.fresh || p.epoch.generation != g {
                continue;
            }
            let cand = (p.since_ms, *k);
            if best.is_none_or(|b| cand < b) {
                best = Some(cand);
            }
        }
        if let Some((rank, k)) = best {
            if k == self.my_key {
                self.try_claim(g, epoch, rank);
            }
        }
    }

    /// Claim the coordinator role for `(g, epoch.term + 1)`. Never twice for one term,
    /// never past the last term, and only when the claim probes as valid and would
    /// make this node the coordinator.
    fn try_claim(&mut self, g: u32, epoch: Epoch, rank: u64) {
        let Some(term) = epoch.term.checked_add(1) else {
            return;
        };
        if self.last_claim == Some((g, term)) {
            return;
        }
        let size = self.grants.as_ref().map_or(0, |x| x.len());
        if self.claim_probe_failed == Some((g, term, size)) {
            return;
        }
        let claim = build::claim(self.signer(), self.room, g, term, rank, self.now.unix_ms);
        let wins = self.probe(&claim).is_ok_and(|p| {
            p.coordinator().is_some_and(|(e, c)| {
                e == Epoch {
                    generation: g,
                    term,
                } && c.id == claim.id
            })
        });
        if !wins {
            self.claim_probe_failed = Some((g, term, size));
            return;
        }
        self.claim_probe_failed = None;
        self.last_claim = Some((g, term));
        let _ = self.commit_local_records(vec![claim]);
    }

    /// Recompute visibility and push the differences to subscribers.
    fn refresh(&mut self) {
        let Some(g) = &self.grants else {
            return;
        };
        let pv = self.presence_views(self.now.mono_us);
        let new = self.store.visible(g, &pv);
        let mut edges = Vec::new();
        for (p, v) in &new {
            if self.published.get(p) != Some(v) {
                edges.extend(self.subs.note(p, Some(v)));
            }
        }
        for p in self.published.keys() {
            if !new.contains_key(p) {
                edges.extend(self.subs.note(p, None));
            }
        }
        self.published = new;
        self.ready(edges);
    }

    // ------------------------------------------------------------------
    // Timers
    // ------------------------------------------------------------------

    fn settle(&mut self) {
        if self.removed {
            return;
        }
        self.timers();
        self.liveness();
        self.refresh();
    }

    /// Earliest grant expiry among this node and its connected peers, as a monotonic
    /// deadline. An expiry already past is due now.
    fn next_expiry_us(&self) -> Option<u64> {
        let g = self.grants.as_ref()?;
        let (now, unix) = (self.now.mono_us, self.now.unix_ms);
        let mut keys = vec![self.my_key];
        keys.extend(
            self.conns
                .values()
                .filter(|c| c.state != CState::Handshake)
                .map(|c| c.peer),
        );
        keys.into_iter()
            .filter_map(|k| g.member_expiry_ms(&k))
            .map(|t| now.saturating_add(t.saturating_sub(unix).saturating_mul(1000)))
            .min()
    }

    /// Re-run admission against the wall clock: grants expire after they were checked.
    fn check_expiry(&mut self) {
        let unix = self.now.unix_ms;
        let Some(g) = self.grants.as_ref() else {
            return;
        };
        if !g.is_member_at(&self.my_key, unix) {
            return self.on_removed();
        }
        let expired: Vec<ConnId> = self
            .conns
            .iter()
            .filter(|(_, c)| c.state != CState::Handshake)
            .filter(|(_, c)| g.is_member(&c.peer) && !g.is_member_at(&c.peer, unix))
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            self.close_conn(id, Reason::Revoked);
        }
    }

    fn timers(&mut self) {
        self.check_expiry();
        if self.removed {
            return;
        }
        let now = self.now.mono_us;
        let late: Vec<ConnId> = self
            .conns
            .iter()
            .filter(|(_, c)| c.state != CState::Established && now >= c.deadline_us)
            .map(|(id, _)| *id)
            .collect();
        for id in late {
            let reason = if self.conns[&id].state == CState::Probation {
                Reason::Closed
            } else {
                Reason::Shutdown
            };
            self.close_conn(id, reason);
        }
        if self.dialing_active() {
            let targets: Vec<Key> = self.dial.keys().copied().collect();
            let redial = self.cfg.timing.redial_ms * 1000;
            for k in targets {
                if self.by_peer.contains_key(&k) {
                    continue;
                }
                let d = self.dial.get_mut(&k).unwrap();
                if d.in_flight_until.is_some_and(|t| now >= t) {
                    d.in_flight_until = None;
                }
                if d.in_flight_until.is_none() && now >= d.next_us {
                    d.in_flight_until = Some(now + DIAL_TIMEOUT_US);
                    d.next_us = now + redial;
                    let hints = self
                        .joining
                        .as_ref()
                        .map(|j| j.token.relays.clone())
                        .unwrap_or_default();
                    self.out.push_back(Output::Dial { peer: k, hints });
                }
            }
        }
        if self.presence_active() && now >= self.next_presence_us {
            self.write_presence(true);
        }
    }

    // ------------------------------------------------------------------
    // Messages
    // ------------------------------------------------------------------

    fn deliver_local(&mut self, from: Key, channel: u16, payload: &[u8]) -> AckStatus {
        let Some(targets) = self.channels.get(&channel) else {
            return AckStatus::NoChannel;
        };
        let targets: Vec<ClientId> = targets.iter().copied().collect();
        if targets.is_empty() {
            return AckStatus::NoChannel;
        }
        for client in targets {
            self.emit_to(
                client,
                LocalEvent::Message {
                    from,
                    channel,
                    payload: payload.to_vec(),
                },
            );
        }
        AckStatus::Delivered
    }

    fn on_message(&mut self, conn: ConnId, msg_id: u64, channel: u16, flags: u8, payload: Vec<u8>) {
        let peer = self.conns[&conn].peer;
        let status = if !self.grants.as_ref().is_some_and(|g| g.is_member(&peer)) {
            AckStatus::NotMember
        } else if payload.len() > MESSAGE_CAP {
            AckStatus::TooLarge
        } else {
            self.deliver_local(peer, channel, &payload)
        };
        if flags & msg_flags::RELIABLE != 0 {
            self.send(conn, Lane::State, Frame::MessageAck { msg_id, status });
        }
    }

    /// Resolve `to` at send time. `(key, conn)`; `conn` is `None` for this node or an
    /// offline member.
    fn resolve(&self, to: &To) -> Result<Vec<(Key, Option<ConnId>)>, ErrorCode> {
        match to {
            To::All => Ok(self
                .established_conns()
                .into_iter()
                .map(|(c, k)| (k, Some(c)))
                .collect()),
            To::Member(k) => {
                if *k == self.my_key {
                    Ok(vec![(*k, None)])
                } else {
                    Ok(vec![(*k, self.established(k))])
                }
            }
            To::Coordinator => {
                let c = self.coord.as_ref().ok_or(ErrorCode::NoCoordinator)?;
                if c.status != CoordStatus::Fresh {
                    return Err(ErrorCode::NoCoordinator);
                }
                if c.holder == self.my_key {
                    Ok(vec![(c.holder, None)])
                } else {
                    Ok(vec![(c.holder, self.established(&c.holder))])
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Local commands
    // ------------------------------------------------------------------

    fn on_local(&mut self, client: ClientId, cmd: LocalCommand) {
        use LocalCommand as C;
        if let C::Attach = cmd {
            self.clients.insert(client);
            match self.lifecycle_seen {
                Lifecycle::Open(generation) => {
                    self.emit_to(client, LocalEvent::RoomOpened { generation })
                }
                Lifecycle::Closed(generation) => {
                    self.emit_to(client, LocalEvent::RoomClosed { generation })
                }
                Lifecycle::Dead => {}
            }
            if let Some(c) = &self.coord {
                let ev = LocalEvent::Coordinator(CoordinatorEvent::Changed {
                    epoch: c.epoch,
                    holder: c.holder,
                });
                self.emit_to(client, ev);
            }
            return;
        }
        if !self.clients.contains(&client) {
            if let Some(req) = cmd_req(&cmd) {
                self.err(client, req, ErrorCode::NotAttached);
            }
            return;
        }
        match cmd {
            C::Attach => {}
            C::Detach => {
                self.clients.remove(&client);
                self.subs.drop_client(client);
                for set in self.channels.values_mut() {
                    set.remove(&client);
                }
            }
            C::Subscribe { req, sub, pattern } => match Pattern::parse(&pattern) {
                None => self.err(client, req, ErrorCode::BadPattern),
                Some(p) => {
                    self.reply(client, req, Reply::Ok);
                    let edges = self.subs.subscribe(client, sub, p, &self.published);
                    self.ready(edges);
                }
            },
            C::Unsubscribe { sub } => self.subs.unsubscribe(client, sub),
            C::Write {
                req,
                scope,
                name,
                class,
                lifetime,
                value,
            } => match self.local_write(scope, name, class, lifetime, value) {
                Ok(()) => self.reply(client, req, Reply::Ok),
                Err(e) => self.err(client, req, e),
            },
            C::RegisterChannel { req, channel } => {
                self.channels.entry(channel).or_default().insert(client);
                self.reply(client, req, Reply::Ok);
            }
            C::UnregisterChannel { channel } => {
                if let Some(s) = self.channels.get_mut(&channel) {
                    s.remove(&client);
                    if s.is_empty() {
                        self.channels.remove(&channel);
                    }
                }
            }
            C::Send {
                req,
                to,
                channel,
                reliable,
                payload,
            } => self.local_send(client, req, to, channel, reliable, payload),
            C::SendDatagram {
                to,
                channel,
                payload,
                max_age_ms,
            } => {
                let Ok(targets) = self.resolve(&to) else {
                    return;
                };
                self.dgram_seq = self.dgram_seq.wrapping_add(1);
                let bytes = Datagram::App {
                    channel,
                    seq: self.dgram_seq,
                    send_ms: (self.now.mono_us / 1000) as u32,
                    payload: payload.clone(),
                }
                .encode();
                for (k, conn) in targets {
                    match conn {
                        Some(conn) => self.out.push_back(Output::SendDatagram {
                            conn,
                            bytes: bytes.clone(),
                            max_age_ms,
                        }),
                        None if k == self.my_key => {
                            if let Some(set) = self.channels.get(&channel) {
                                let targets: Vec<ClientId> = set.iter().copied().collect();
                                for c in targets {
                                    self.emit_to(
                                        c,
                                        LocalEvent::Datagram {
                                            from: k,
                                            channel,
                                            payload: payload.clone(),
                                            age_ms: 0,
                                        },
                                    );
                                }
                            }
                        }
                        None => {}
                    }
                }
            }
            C::Status { req } => {
                let st = self.status(self.now);
                self.reply(client, req, Reply::Status(Box::new(st)));
            }
            C::CreateInvite {
                req,
                expiry_ms,
                relays,
            } => match self.local_invite(expiry_ms, relays) {
                Ok(token) => self.reply(client, req, Reply::Invite { token }),
                Err(e) => self.err(client, req, e),
            },
            C::Grant {
                req,
                kind,
                subject,
                expiry_ms,
            } => match self.local_grant(kind, subject, expiry_ms) {
                Ok(()) => self.reply(client, req, Reply::Ok),
                Err(e) => self.err(client, req, e),
            },
            C::Revoke { req, subject } => match self.local_revoke(subject, None) {
                Ok(()) => self.reply(client, req, Reply::Ok),
                Err(e) => self.err(client, req, e),
            },
            C::Retire {
                req,
                subject,
                keep_all,
            } => match self.local_revoke(subject, Some(keep_all)) {
                Ok(()) => self.reply(client, req, Reply::Ok),
                Err(e) => self.err(client, req, e),
            },
            C::Close { req } => match self.local_close() {
                Ok(()) => self.reply(client, req, Reply::Ok),
                Err(e) => self.err(client, req, e),
            },
            C::Reopen { req } => match self.local_reopen() {
                Ok(()) => self.reply(client, req, Reply::Ok),
                Err(e) => self.err(client, req, e),
            },
            C::Leave { req } => {
                if self.grants.is_none() {
                    return self.err(client, req, ErrorCode::NoRoom);
                }
                if self.is_member_open() {
                    self.write_presence(false);
                }
                self.left = true;
                self.close_all(Reason::Leave);
                self.reply(client, req, Reply::Ok);
            }
        }
    }

    fn local_write(
        &mut self,
        scope: Scope,
        name: String,
        class: Class,
        lifetime: Lifetime,
        value: Vec<u8>,
    ) -> Result<(), ErrorCode> {
        let g = self.grants.as_ref().ok_or(ErrorCode::NoRoom)?;
        let Lifecycle::Open(generation) = g.lifecycle() else {
            return Err(ErrorCode::Closed);
        };
        if !g.is_member(&self.my_key) || self.left {
            return Err(ErrorCode::NotAuthorized);
        }
        if !valid_name(&name)
            || (scope == Scope::Member && class == Class::Durable && name == PRESENCE)
        {
            return Err(ErrorCode::BadName);
        }
        let cap = if class == Class::Live {
            LIVE_VALUE_CAP
        } else {
            DURABLE_VALUE_CAP
        };
        if value.len() > cap {
            return Err(ErrorCode::TooLarge);
        }
        let version = match scope {
            Scope::Member => {
                let seq = self.bump_seq().ok_or(ErrorCode::Unknown)?;
                Version {
                    generation,
                    term: self.cfg.member_incarnation,
                    seq,
                }
            }
            Scope::Room => {
                let (epoch, holder) = {
                    let (e, c) = g.coordinator().ok_or(ErrorCode::NoCoordinator)?;
                    (e, c.holder)
                };
                if holder != self.my_key {
                    return Err(ErrorCode::NotCoordinator);
                }
                let mine = self.store.max_room_seq(epoch, self.my_key);
                let s = self.room_seq.entry(epoch).or_insert(0);
                *s = (*s).max(mine).checked_add(1).ok_or(ErrorCode::Unknown)?;
                Version {
                    generation: epoch.generation,
                    term: epoch.term,
                    seq: *s,
                }
            }
        };
        let rec = StateRecord::build(
            &self.me, self.room, class, lifetime, scope, version, &name, &value,
        )
        .map_err(|_| ErrorCode::BadName)?;
        match self.commit_own_state(rec) {
            Insert::Improved => Ok(()),
            Insert::Rejected("over budget") => Err(ErrorCode::OverBudget),
            Insert::Orphan => Err(ErrorCode::NotCoordinator),
            _ => Err(ErrorCode::Unknown),
        }
    }

    fn local_send(
        &mut self,
        client: ClientId,
        req: ReqId,
        to: To,
        channel: u16,
        reliable: bool,
        payload: Vec<u8>,
    ) {
        if self.grants.is_none() {
            return self.err(client, req, ErrorCode::NoRoom);
        }
        if payload.len() > MESSAGE_CAP {
            return self.err(client, req, ErrorCode::TooLarge);
        }
        let targets = match self.resolve(&to) {
            Ok(t) => t,
            Err(e) => return self.err(client, req, e),
        };
        let msg_id = self.next_msg_id;
        self.next_msg_id += 1;
        self.reply(
            client,
            req,
            Reply::Sending {
                msg_id,
                targets: targets.iter().map(|(k, _)| *k).collect(),
            },
        );
        let target = match to {
            To::Member(_) => Target::Receiver,
            To::All => Target::Broadcast,
            To::Coordinator => Target::Coordinator,
        };
        for (k, conn) in targets {
            let status = match conn {
                None if k == self.my_key => {
                    MessageStatus::Acked(self.deliver_local(k, channel, &payload))
                }
                None => MessageStatus::Offline,
                Some(conn) => {
                    self.send(
                        conn,
                        Lane::State,
                        Frame::Message {
                            msg_id,
                            channel,
                            flags: if reliable { msg_flags::RELIABLE } else { 0 },
                            target,
                            payload: payload.clone(),
                        },
                    );
                    if reliable {
                        self.pending_msgs.insert((conn, msg_id), (client, k));
                        continue;
                    }
                    MessageStatus::Acked(AckStatus::Delivered)
                }
            };
            self.emit_to(
                client,
                LocalEvent::MessageResult {
                    msg_id,
                    to: k,
                    status,
                },
            );
        }
    }

    fn require_keyholder(&self) -> Result<u32, ErrorCode> {
        let g = self.grants.as_ref().ok_or(ErrorCode::NoRoom)?;
        if self.removed || !g.is_keyholder(&self.my_key) {
            return Err(ErrorCode::NotAuthorized);
        }
        match g.lifecycle() {
            Lifecycle::Open(n) | Lifecycle::Closed(n) => Ok(n),
            Lifecycle::Dead => Err(ErrorCode::Closed),
        }
    }

    fn local_invite(&mut self, expiry_ms: u64, relays: Vec<String>) -> Result<String, ErrorCode> {
        let g = self.grants.as_ref().ok_or(ErrorCode::NoRoom)?;
        let Lifecycle::Open(generation) = g.lifecycle() else {
            return Err(ErrorCode::Closed);
        };
        if !(g.is_keyholder(&self.my_key) || g.is_inviter(&self.my_key)) {
            return Err(ErrorCode::NotAuthorized);
        }
        if relays.len() > RELAY_CAP || relays.iter().any(|r| r.len() > RELAY_LEN_CAP) {
            return Err(ErrorCode::TooLarge);
        }
        let mut peers = vec![self.my_key];
        peers.extend(self.connected_peers().into_iter().take(PEER_CAP - 1));
        let secret: [u8; 32] = self.entropy.bytes();
        let nonce = self.nonce16();
        let tok = InviteToken::mint(
            self.signer(),
            self.room,
            generation,
            secret,
            nonce,
            self.now.unix_ms,
            expiry_ms,
            peers,
            relays,
        );
        let invite = tok.invite.clone();
        if !self.probe(&invite)?.is_valid(&invite.id) {
            return Err(ErrorCode::NotAuthorized);
        }
        self.commit_local_records(vec![invite])?;
        Ok(tok.encode())
    }

    fn local_grant(&mut self, kind: Kind, subject: Key, expiry_ms: u64) -> Result<(), ErrorCode> {
        let g = self.grants.as_ref().ok_or(ErrorCode::NoRoom)?;
        let allowed = match kind {
            Kind::Keyholder => self.my_key == g.root(),
            Kind::Inviter | Kind::Member => g.is_keyholder(&self.my_key),
            _ => false,
        };
        if !allowed || self.removed {
            return Err(ErrorCode::NotAuthorized);
        }
        let cites = g.latest_revocation_of(&subject);
        let nonce = self.nonce16();
        let rec = build::grant(
            self.signer(),
            self.room,
            kind,
            subject,
            nonce,
            self.now.unix_ms,
            expiry_ms,
            0,
            cites,
        );
        if !self.probe(&rec)?.is_valid(&rec.id) {
            return Err(ErrorCode::NotAuthorized);
        }
        self.commit_local_records(vec![rec])
    }

    /// `retire = None` issues a revoke; `Some(keep_all)` a retire.
    fn local_revoke(&mut self, subject: Key, retire: Option<bool>) -> Result<(), ErrorCode> {
        let nonce = self.nonce16();
        let g = self.grants.as_ref().ok_or(ErrorCode::NoRoom)?;
        if self.removed || !(g.is_keyholder(&self.my_key) || subject == self.my_key) {
            return Err(ErrorCode::NotAuthorized);
        }
        let cites = g.latest_revocation_of(&subject);
        let rec = match retire {
            None => build::revoke(
                self.signer(),
                self.room,
                subject,
                nonce,
                self.now.unix_ms,
                cites,
            ),
            Some(keep_all) => {
                let kept: Vec<RecordId> = if keep_all {
                    g.records()
                        .filter(|r| r.issuer == subject && g.is_valid(&r.id))
                        .map(|r| r.id)
                        .collect()
                } else {
                    Vec::new()
                };
                if kept.len() > crate::record::KEPT_CAP {
                    return Err(ErrorCode::TooLarge);
                }
                build::retire(
                    self.signer(),
                    self.room,
                    subject,
                    nonce,
                    self.now.unix_ms,
                    cites,
                    kept,
                )
            }
        };
        // Would it take effect? (A keyholder can't remove a keyholder or root.)
        if !self
            .probe(&rec)?
            .authority()
            .effective_revocations
            .contains(&rec.id)
        {
            return Err(ErrorCode::NotAuthorized);
        }
        self.commit_local_records(vec![rec])
    }

    fn local_close(&mut self) -> Result<(), ErrorCode> {
        let n = self.require_keyholder()?;
        let g = self.grants.as_ref().unwrap();
        if !matches!(g.lifecycle(), Lifecycle::Open(_)) {
            return Err(ErrorCode::Closed);
        }
        let rec = build::close(self.signer(), self.room, n, self.now.unix_ms);
        if self.probe(&rec)?.lifecycle() != Lifecycle::Closed(n) {
            return Err(ErrorCode::NotAuthorized);
        }
        self.commit_local_records(vec![rec])
    }

    fn local_reopen(&mut self) -> Result<(), ErrorCode> {
        let n = self.require_keyholder()?;
        let g = self.grants.as_ref().unwrap();
        if !matches!(g.lifecycle(), Lifecycle::Closed(_)) {
            return Err(ErrorCode::Unknown);
        }
        let next = n.checked_add(1).ok_or(ErrorCode::Unknown)?;
        let nonce = self.nonce16();
        let rec = build::open(self.signer(), self.room, next, nonce, self.now.unix_ms);
        if self.probe(&rec)?.lifecycle() != Lifecycle::Open(next) {
            return Err(ErrorCode::NotAuthorized);
        }
        self.commit_local_records(vec![rec])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Timing;
    use crate::frame::caps;
    use crate::grants::MAX_RECORDS;

    const U0: u64 = 1_800_000_000_000;

    fn cfg(seed: u8) -> Config {
        Config {
            identity: Identity::from_secret([seed; 32]),
            member_incarnation: 1,
            daemon_incarnation: 1,
            entropy_seed: [seed; 32],
            timing: Timing::default(),
            caps: caps::R1,
        }
    }

    fn at(ms: u64) -> Now {
        Now {
            mono_us: 1_000_000 + ms * 1000,
            unix_ms: U0 + ms,
        }
    }

    fn key_of(seed: u8) -> Key {
        Identity::from_secret([seed; 32]).key()
    }

    /// Copy every record `from` has that `to` lacks.
    fn sync_records(from: &Room, to: &mut Room) {
        let recs: Vec<Record> = from.grants().unwrap().records().cloned().collect();
        to.grants.as_mut().unwrap().insert_all(recs);
    }

    #[test]
    fn removed_follows_the_grant_set_both_ways() {
        let mut root = Room::found(cfg(1), at(0), b"m");
        let b = key_of(2);
        root.local_grant(Kind::Member, b, 0).unwrap();
        let mut joiner = Room::resume(cfg(2), root.persist(), at(1)).unwrap();
        assert!(!joiner.is_removed());

        root.local_revoke(b, None).unwrap();
        sync_records(&root, &mut joiner);
        joiner.reconcile();
        assert!(joiner.is_removed(), "revoked");

        // A re-admission that cites the revocation heals it.
        root.local_grant(Kind::Member, b, 0).unwrap();
        sync_records(&root, &mut joiner);
        joiner.reconcile();
        assert!(!joiner.is_removed(), "re-admitted");
        assert!(joiner.grants().unwrap().is_member(&b));
        // And it is live again: dials and presence resume.
        joiner.handle(at(2), Input::Tick);
        assert!(joiner.poll_timeout().is_some());
    }

    #[test]
    fn claim_term_overflow_does_not_claim_or_panic() {
        let mut r = Room::found(cfg(1), at(0), b"m");
        let n = r.grants().unwrap().len();
        r.try_claim(
            1,
            Epoch {
                generation: 1,
                term: u32::MAX,
            },
            0,
        );
        assert_eq!(r.grants().unwrap().len(), n);
        assert_eq!(r.last_claim, None);
    }

    #[test]
    fn a_term_is_claimed_once() {
        let mut r = Room::found(cfg(1), at(0), b"m");
        let n = r.grants().unwrap().len();
        let epoch = Epoch {
            generation: 1,
            term: 0,
        };
        r.try_claim(1, epoch, 0);
        assert_eq!(r.grants().unwrap().len(), n + 1);
        assert_eq!(r.coordinator().map(|(e, _)| e.term), Some(1));
        r.try_claim(1, epoch, 0);
        r.try_claim(1, epoch, 5);
        assert_eq!(
            r.grants().unwrap().len(),
            n + 1,
            "no second claim for term 1"
        );
    }

    #[test]
    fn an_invalid_claim_is_probed_and_not_committed() {
        let root = Room::found(cfg(1), at(0), b"m");
        // Not a member: its claim would not count.
        let mut outsider = Room::resume(cfg(9), root.persist(), at(1)).unwrap();
        let n = outsider.grants().unwrap().len();
        outsider.try_claim(
            1,
            Epoch {
                generation: 1,
                term: 0,
            },
            0,
        );
        assert_eq!(outsider.grants().unwrap().len(), n);
        assert_eq!(outsider.last_claim, None);
    }

    #[test]
    fn grant_that_cannot_take_effect_is_refused() {
        let root_id = Identity::from_secret([1; 32]);
        let mut r = Room::found(cfg(1), at(0), b"m");
        let (k, s) = (key_of(2), key_of(3));
        r.local_grant(Kind::Keyholder, k, 0).unwrap();
        r.local_grant(Kind::Member, s, 0).unwrap();
        // Root and the keyholder revoke the member concurrently: neither cites the
        // other, so no single cite can re-admit it (they are final in v1).
        let room = r.room_id();
        let k_id = Identity::from_secret([2; 32]);
        let revs = vec![
            build::revoke(&root_id, room, s, [1; 16], U0, RecordId::ZERO),
            build::revoke(&k_id, room, s, [2; 16], U0, RecordId::ZERO),
        ];
        r.grants.as_mut().unwrap().insert_all(revs);
        r.reconcile();
        let n = r.grants().unwrap().len();
        assert_eq!(
            r.local_grant(Kind::Member, s, 0),
            Err(ErrorCode::NotAuthorized)
        );
        assert_eq!(r.grants().unwrap().len(), n, "nothing was committed");
    }

    #[test]
    fn full_grant_set_refuses_instead_of_panicking() {
        let root_id = Identity::from_secret([1; 32]);
        let mut r = Room::found(cfg(1), at(0), b"m");
        let room = r.room_id();
        let filler: Vec<Record> = (0..MAX_RECORDS as u32)
            .map(|i| {
                let mut subject = [0u8; 32];
                subject[..4].copy_from_slice(&i.to_le_bytes());
                let mut nonce = [0u8; 16];
                nonce[..4].copy_from_slice(&i.to_le_bytes());
                build::grant(
                    &root_id,
                    room,
                    Kind::Invite,
                    Key(subject),
                    nonce,
                    U0,
                    0,
                    1,
                    RecordId::ZERO,
                )
            })
            .collect();
        r.grants.as_mut().unwrap().insert_all(filler);
        assert_eq!(r.grants().unwrap().len(), MAX_RECORDS + 1);

        assert_eq!(
            r.local_grant(Kind::Member, key_of(5), 0),
            Err(ErrorCode::OverBudget)
        );
        assert_eq!(r.local_invite(0, Vec::new()), Err(ErrorCode::OverBudget));
        assert_eq!(r.local_close(), Err(ErrorCode::OverBudget));
        assert_eq!(r.local_revoke(key_of(5), None), Err(ErrorCode::OverBudget));

        // A peer's flood is counted and dropped.
        let extra = build::grant(
            &root_id,
            room,
            Kind::Inviter,
            key_of(6),
            [0xee; 16],
            U0,
            0,
            0,
            RecordId::ZERO,
        );
        let before = r.stats().rejected_records;
        r.on_records(7, vec![extra.bytes.clone()]);
        assert_eq!(r.stats().rejected_records, before + 1);
        assert_eq!(r.grants().unwrap().len(), MAX_RECORDS + 1);
        // So is our own claim.
        let claim = build::claim(&root_id, room, 1, 1, 0, U0);
        assert_eq!(
            r.commit_local_records(vec![claim]),
            Err(ErrorCode::OverBudget)
        );
    }

    #[test]
    fn member_expiry_is_a_deadline_and_removes_self() {
        let mut root = Room::found(cfg(1), at(0), b"m");
        let b = key_of(2);
        root.local_grant(Kind::Member, b, U0 + 30_000).unwrap();
        let mut member = Room::resume(cfg(2), root.persist(), at(1)).unwrap();
        assert!(!member.is_removed());
        assert_eq!(member.next_expiry_us(), Some(at(1).mono_us + 29_999_000));
        // The deadline is a real timer deadline, not only a presence one.
        member.handle(at(10_000), Input::Tick);
        assert!(member
            .poll_timeout()
            .is_some_and(|t| t <= at(30_000).mono_us));

        // An already-due expiry is reported too (not filtered as "past").
        member.now = at(40_000);
        assert!(member
            .poll_timeout()
            .is_some_and(|t| t <= member.now.mono_us));
        member.handle(at(40_000), Input::Tick);
        assert!(member.is_removed(), "own grant expired");
        assert_eq!(member.poll_timeout(), None);
    }

    #[test]
    fn expired_member_may_redeem_a_fresh_invite() {
        let mut root = Room::found(cfg(1), at(0), b"m");
        let b = key_of(2);
        root.local_grant(Kind::Member, b, U0 + 1_000).unwrap();
        root.handle(at(5_000), Input::Tick);
        let room = root.room_id();
        let hello = |proof: Vec<Vec<u8>>| Hello {
            version: PROTOCOL_VERSION,
            caps: caps::R1,
            room,
            member: b,
            daemon_incarnation: 1,
            open_generation: 1,
            proof,
        };
        assert_eq!(
            root.check_dialler(b, &hello(Vec::new())).map(|(s, _)| s),
            Err(Reason::NotMember)
        );
        let token = root.local_invite(0, Vec::new()).unwrap();
        let tok = InviteToken::decode(&token).unwrap();
        let proof = tok.redeem(b, [9; 16], U0);
        let proof: Vec<Vec<u8>> = proof.iter().map(|r| r.bytes.clone()).collect();
        let (state, adopted) = root.check_dialler(b, &hello(proof)).unwrap();
        assert_eq!(state, CState::Established);
        assert_eq!(adopted.len(), 2);
        assert!(root.grants().unwrap().admit(&b, root.now.unix_ms).is_ok());
    }

    #[test]
    fn state_frames_that_overtake_the_hello_are_held_and_replayed() {
        let mut root = Room::found(cfg(1), at(0), b"m");
        let b = key_of(2);
        root.local_grant(Kind::Member, b, 0).unwrap();
        let rec = StateRecord::build(
            &Identity::from_secret([2; 32]),
            root.room_id(),
            Class::Durable,
            Lifetime::Persistent,
            Scope::Member,
            Version {
                generation: 1,
                term: 1,
                seq: 1,
            },
            "kv/a",
            b"early",
        )
        .unwrap();
        let conn = 5;
        root.handle(
            at(1),
            Input::Connected {
                conn,
                peer: b,
                outbound: false,
            },
        );
        // The state lane delivers first, then the hello on the control lane.
        root.handle(
            at(2),
            Input::Frame {
                conn,
                lane: Lane::State,
                bytes: Frame::State(rec.bytes.clone()).encode(),
            },
        );
        assert!(root.store.best(&Slot::of(&rec)).is_none());
        let hello = Frame::Hello(Hello {
            version: PROTOCOL_VERSION,
            caps: caps::R1,
            room: root.room_id(),
            member: b,
            daemon_incarnation: 1,
            open_generation: 1,
            proof: Vec::new(),
        });
        root.handle(
            at(3),
            Input::Frame {
                conn,
                lane: Lane::Control,
                bytes: hello.encode(),
            },
        );
        assert_eq!(root.connected_peers(), vec![b]);
        assert_eq!(
            root.store.best(&Slot::of(&rec)).map(|r| r.bytes.clone()),
            Some(rec.bytes.clone())
        );
    }
}
