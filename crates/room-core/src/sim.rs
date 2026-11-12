//! Deterministic N-node simulator (ROOM.md "Simulator").
//!
//! Drives the same [`Room`] the daemon does through an in-memory network: one
//! ordered, reliable link per (pair, lane) with seeded latency and jitter (lanes
//! reorder against each other), unordered lossy datagrams, partitions, and node
//! stop / restart. Time is virtual: the scheduler jumps to the earlier of the next
//! delivery and the nodes' `poll_timeout`. Every random choice comes from the run
//! seed, so the same seed replays the same event trace ([`Sim::trace_hash`]).

use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap};

use crate::api::*;
use crate::frame::Frame;
use crate::grants::Lifecycle;
use crate::ids::{Identity, Key, Now};
use crate::room::{Persisted, Room};
use crate::state::Vis;
use crate::token::InviteToken;
use crate::wire::h;

/// splitmix64: tiny, fast, good enough for scheduling.
#[derive(Clone, Debug)]
pub struct SplitMix(pub u64);

impl SplitMix {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    /// Uniform in `0..n` (`n > 0`).
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
    /// True with probability `pct`%.
    pub fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
}

#[derive(Clone, Debug)]
pub struct NetConfig {
    pub base_latency_ms: u64,
    pub jitter_ms: u64,
    pub dgram_loss_pct: u64,
    /// How long a severed link takes to be noticed by its endpoints.
    pub detect_ms: u64,
    pub dial_fail_ms: u64,
    pub timing: Timing,
}

impl Default for NetConfig {
    fn default() -> Self {
        Self {
            base_latency_ms: 20,
            jitter_ms: 30,
            dgram_loss_pct: 10,
            detect_ms: 3_000,
            dial_fail_ms: 300,
            timing: Timing::default(),
        }
    }
}

enum Ev {
    Frame {
        node: usize,
        conn: ConnId,
        lane: Lane,
        bytes: Vec<u8>,
    },
    Datagram {
        node: usize,
        conn: ConnId,
        bytes: Vec<u8>,
    },
    Up {
        node: usize,
        conn: ConnId,
        peer: Key,
        outbound: bool,
    },
    Down {
        node: usize,
        conn: ConnId,
    },
    DialFail {
        node: usize,
        peer: Key,
    },
}

struct Sched {
    at: u64,
    seq: u64,
    ev: Ev,
}
impl PartialEq for Sched {
    fn eq(&self, o: &Self) -> bool {
        (self.at, self.seq) == (o.at, o.seq)
    }
}
impl Eq for Sched {}
impl PartialOrd for Sched {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Sched {
    fn cmp(&self, o: &Self) -> Ordering {
        // Min-heap on (at, seq).
        (o.at, o.seq).cmp(&(self.at, self.seq))
    }
}

struct Link {
    a: usize,
    b: usize,
    up: bool,
    /// Last scheduled delivery per (destination node, lane): keeps each lane ordered.
    last: BTreeMap<(usize, u8), u64>,
}

pub struct SimNode {
    pub identity: Identity,
    pub room: Option<Room>,
    pub up: bool,
    pub incarnation: u32,
    persisted: Option<Persisted>,
    /// Every `LocalEvent` the node's client received, in order.
    pub events: Vec<LocalEvent>,
    /// Subscription updates, drained as soon as `Ready` arrives, by sub id.
    pub updates: BTreeMap<u32, Vec<Update>>,
    mono_off: u64,
    skew_ms: i64,
    daemon_inc: u64,
}

pub const CLIENT: ClientId = 1;
const BASE_UNIX_MS: u64 = 1_800_000_000_000;

pub struct Sim {
    pub rng: SplitMix,
    pub cfg: NetConfig,
    pub now_us: u64,
    pub nodes: Vec<SimNode>,
    seed: u64,
    heap: BinaryHeap<Sched>,
    seq: u64,
    links: BTreeMap<u64, Link>,
    next_link: u64,
    group: Vec<u32>,
    next_req: u64,
    trace: blake3::Hasher,
}

impl Sim {
    /// `n` nodes with identities derived from `seed`; node 0 founds the room, the
    /// others exist but have no room until [`Sim::join`].
    pub fn new(seed: u64, n: usize) -> Sim {
        Sim::with_config(seed, n, NetConfig::default())
    }

    pub fn with_config(seed: u64, n: usize, cfg: NetConfig) -> Sim {
        let mut rng = SplitMix(seed);
        let nodes = (0..n)
            .map(|i| {
                let secret = h(
                    "sim identity",
                    &[seed.to_le_bytes(), (i as u64).to_le_bytes()].concat(),
                );
                SimNode {
                    identity: Identity::from_secret(secret),
                    room: None,
                    up: true,
                    incarnation: 1,
                    persisted: None,
                    events: Vec::new(),
                    updates: BTreeMap::new(),
                    mono_off: 1_000_000_000 + rng.below(1_000_000_000),
                    skew_ms: rng.below(400) as i64 - 200,
                    daemon_inc: rng.next_u64(),
                }
            })
            .collect();
        let mut sim = Sim {
            rng,
            cfg,
            now_us: 1_000_000,
            nodes,
            seed,
            heap: BinaryHeap::new(),
            seq: 0,
            links: BTreeMap::new(),
            next_link: 1,
            group: vec![0; n],
            next_req: 1,
            trace: blake3::Hasher::new(),
        };
        let cfg0 = sim.config(0);
        let now = sim.now_of(0);
        sim.nodes[0].room = Some(Room::found(cfg0, now, b"sim room"));
        sim.pump(0);
        sim.attach(0);
        sim
    }

    /// A founder plus `n - 1` members, all joined through tokens and connected.
    pub fn bootstrap(seed: u64, n: usize) -> Sim {
        let mut sim = Sim::new(seed, n);
        // Node 0 is root. Every other new member also becomes an inviter with even
        // odds, so later joiners enter through varied bootstrap peers.
        let mut inviters = vec![0usize];
        for i in 1..n {
            let via = inviters[sim.rng.below(inviters.len() as u64) as usize];
            sim.join(i, via);
            sim.run_for(2_000);
            if sim.rng.chance(50) {
                let subject = sim.key(i);
                let r = sim.request(0, |req| LocalCommand::Grant {
                    req,
                    kind: crate::record::Kind::Inviter,
                    subject,
                    expiry_ms: 0,
                });
                assert_eq!(r, Reply::Ok);
                sim.run_for(2_000);
                inviters.push(i);
            }
        }
        sim.run_for(15_000);
        sim
    }

    fn config(&self, i: usize) -> Config {
        let n = &self.nodes[i];
        Config {
            identity: n.identity.clone(),
            member_incarnation: n.incarnation,
            daemon_incarnation: n.daemon_inc.wrapping_add(n.incarnation as u64),
            entropy_seed: h(
                "sim entropy",
                &[
                    self.seed.to_le_bytes().as_slice(),
                    &(i as u64).to_le_bytes(),
                    &n.incarnation.to_le_bytes(),
                ]
                .concat(),
            ),
            timing: self.cfg.timing.clone(),
            caps: crate::frame::caps::R1,
        }
    }

    pub fn now_of(&self, i: usize) -> Now {
        let n = &self.nodes[i];
        Now {
            mono_us: self.now_us + n.mono_off,
            unix_ms: (BASE_UNIX_MS + self.now_us / 1000).wrapping_add_signed(n.skew_ms),
        }
    }

    pub fn key(&self, i: usize) -> Key {
        self.nodes[i].identity.key()
    }

    pub fn index_of(&self, k: &Key) -> Option<usize> {
        self.nodes.iter().position(|n| n.identity.key() == *k)
    }

    pub fn room(&self, i: usize) -> &Room {
        self.nodes[i].room.as_ref().expect("node has no room")
    }

    fn attach(&mut self, i: usize) {
        self.input(
            i,
            Input::Local {
                client: CLIENT,
                cmd: LocalCommand::Attach,
            },
        );
    }

    // ------------------------------------------------------------------
    // Driving
    // ------------------------------------------------------------------

    fn trace(&mut self, line: &str) {
        self.trace.update(line.as_bytes());
        self.trace.update(b"\n");
    }

    /// Hash of everything that happened: events, deliveries, dials. Same seed and
    /// same scripted calls give the same hash.
    pub fn trace_hash(&self) -> [u8; 32] {
        *self.trace.finalize().as_bytes()
    }

    fn schedule(&mut self, at: u64, ev: Ev) {
        self.seq += 1;
        self.heap.push(Sched {
            at,
            seq: self.seq,
            ev,
        });
    }

    fn latency_us(&mut self) -> u64 {
        (self.cfg.base_latency_ms + self.rng.below(self.cfg.jitter_ms + 1)) * 1000
    }

    /// Feed one input to node `i` and carry out what it asks for.
    pub fn input(&mut self, i: usize, input: Input) {
        if !self.nodes[i].up {
            return;
        }
        let now = self.now_of(i);
        let Some(room) = self.nodes[i].room.as_mut() else {
            return;
        };
        room.handle(now, input);
        self.pump(i);
    }

    fn pump(&mut self, i: usize) {
        loop {
            let Some(room) = self.nodes[i].room.as_mut() else {
                return;
            };
            let Some(out) = room.poll_output() else {
                return;
            };
            match out {
                Output::Dial { peer, .. } => self.dial(i, peer),
                Output::Send { conn, lane, bytes } => self.send(i, conn, lane, bytes),
                Output::SendDatagram { conn, bytes, .. } => {
                    if self.rng.chance(self.cfg.dgram_loss_pct) {
                        continue;
                    }
                    if let Some((to, true)) = self.far_end(i, conn) {
                        let at = self.now_us + self.latency_us();
                        self.schedule(
                            at,
                            Ev::Datagram {
                                node: to,
                                conn,
                                bytes,
                            },
                        );
                    }
                }
                Output::Close { conn, reason } => self.close(i, conn, reason),
                Output::Local { client, event } => {
                    if client != CLIENT {
                        continue;
                    }
                    let line = format!("{} n{} {:?}", self.now_us, i, event);
                    self.trace(&line);
                    if let LocalEvent::Ready { sub } = &event {
                        let sub = *sub;
                        let ups =
                            self.nodes[i]
                                .room
                                .as_mut()
                                .unwrap()
                                .drain(CLIENT, sub, usize::MAX);
                        let line = format!("{} n{} sub{} {:?}", self.now_us, i, sub, ups);
                        self.trace(&line);
                        self.nodes[i].updates.entry(sub).or_default().extend(ups);
                    }
                    self.nodes[i].events.push(event);
                }
                Output::PersistDirty => {
                    let p = self.nodes[i].room.as_ref().unwrap().persist();
                    self.nodes[i].persisted = Some(p);
                }
            }
        }
    }

    /// The other end of `conn` as seen from `i`, and whether the link carries traffic.
    fn far_end(&self, i: usize, conn: ConnId) -> Option<(usize, bool)> {
        let l = self.links.get(&conn)?;
        let to = if l.a == i { l.b } else { l.a };
        let ok = l.up && self.group[i] == self.group[to] && self.nodes[to].up;
        Some((to, ok))
    }

    fn dial(&mut self, i: usize, peer: Key) {
        let target = self.index_of(&peer);
        let ok = target.is_some_and(|j| {
            j != i
                && self.nodes[j].up
                && self.nodes[j].room.is_some()
                && self.group[i] == self.group[j]
        });
        if !ok {
            let at = self.now_us + self.cfg.dial_fail_ms * 1000 + self.rng.below(100_000);
            self.schedule(at, Ev::DialFail { node: i, peer });
            let line = format!("{} dialfail n{} {:?}", self.now_us, i, peer);
            self.trace(&line);
            return;
        }
        let j = target.unwrap();
        let conn = self.next_link;
        self.next_link += 1;
        self.links.insert(
            conn,
            Link {
                a: i,
                b: j,
                up: true,
                last: BTreeMap::new(),
            },
        );
        let at = self.now_us + self.latency_us();
        let (ki, kj) = (self.key(i), self.key(j));
        self.schedule(
            at,
            Ev::Up {
                node: i,
                conn,
                peer: kj,
                outbound: true,
            },
        );
        self.schedule(
            at,
            Ev::Up {
                node: j,
                conn,
                peer: ki,
                outbound: false,
            },
        );
        let line = format!("{} dial n{} -> n{} conn{}", self.now_us, i, j, conn);
        self.trace(&line);
    }

    fn send(&mut self, i: usize, conn: ConnId, lane: Lane, bytes: Vec<u8>) {
        let Some((to, ok)) = self.far_end(i, conn) else {
            return;
        };
        if !ok {
            return;
        }
        let lat = self.latency_us();
        let l = self.links.get_mut(&conn).unwrap();
        let last = l.last.entry((to, lane as u8)).or_insert(0);
        let at = (self.now_us + lat).max(*last + 1);
        *last = at;
        self.schedule(
            at,
            Ev::Frame {
                node: to,
                conn,
                lane,
                bytes,
            },
        );
    }

    /// `i` closed `conn` after flushing: the far end sees a goodbye, then a disconnect.
    fn close(&mut self, i: usize, conn: ConnId, reason: crate::frame::Reason) {
        let Some((to, ok)) = self.far_end(i, conn) else {
            return;
        };
        if ok {
            let lat = self.latency_us();
            let l = self.links.get_mut(&conn).unwrap();
            let mut at = self.now_us + lat;
            for lane in 0..3u8 {
                if let Some(t) = l.last.get(&(to, lane)) {
                    at = at.max(*t + 1);
                }
            }
            l.up = false;
            self.schedule(
                at,
                Ev::Frame {
                    node: to,
                    conn,
                    lane: Lane::Control,
                    bytes: Frame::Goodbye { reason }.encode(),
                },
            );
            self.schedule(at + 1, Ev::Down { node: to, conn });
        } else if let Some(l) = self.links.get_mut(&conn) {
            l.up = false;
        }
    }

    fn deliver(&mut self, ev: Ev) {
        match ev {
            Ev::Frame {
                node,
                conn,
                lane,
                bytes,
            } => {
                let line = format!(
                    "{} frame n{} c{} l{} {}",
                    self.now_us,
                    node,
                    conn,
                    lane as u8,
                    crate::wire::hex(&h("sim trace", &bytes)[..8])
                );
                self.trace(&line);
                self.input(node, Input::Frame { conn, lane, bytes });
            }
            Ev::Datagram { node, conn, bytes } => {
                if self.links.get(&conn).is_some_and(|l| l.up) {
                    self.input(node, Input::Datagram { conn, bytes });
                }
            }
            Ev::Up {
                node,
                conn,
                peer,
                outbound,
            } => {
                // The link may have been severed while the handshake was in flight.
                let alive = self.links.get(&conn).is_some_and(|l| l.up);
                if alive && self.nodes[node].up {
                    self.input(
                        node,
                        Input::Connected {
                            conn,
                            peer,
                            outbound,
                        },
                    );
                } else if outbound && self.nodes[node].up {
                    self.input(node, Input::DialFailed { peer });
                }
            }
            Ev::Down { node, conn } => {
                let line = format!("{} down n{} c{}", self.now_us, node, conn);
                self.trace(&line);
                self.input(node, Input::Disconnected { conn });
            }
            Ev::DialFail { node, peer } => self.input(node, Input::DialFailed { peer }),
        }
    }

    /// Advance virtual time by `ms`, delivering everything that comes due.
    pub fn run_for(&mut self, ms: u64) {
        let target = self.now_us + ms * 1000;
        loop {
            let mut next = self.heap.peek().map(|s| s.at);
            for n in self.nodes.iter() {
                if !n.up {
                    continue;
                }
                if let Some(t) = n.room.as_ref().and_then(|r| r.poll_timeout()) {
                    let t = t.saturating_sub(n.mono_off);
                    next = Some(next.map_or(t, |x: u64| x.min(t)));
                }
            }
            let Some(t) = next else {
                self.now_us = target;
                return;
            };
            if t > target {
                self.now_us = target;
                return;
            }
            self.now_us = self.now_us.max(t);
            while self.heap.peek().is_some_and(|s| s.at <= self.now_us) {
                let s = self.heap.pop().unwrap();
                self.deliver(s.ev);
            }
            for i in 0..self.nodes.len() {
                if !self.nodes[i].up {
                    continue;
                }
                let due = self.nodes[i]
                    .room
                    .as_ref()
                    .and_then(|r| r.poll_timeout())
                    .is_some_and(|t| t.saturating_sub(self.nodes[i].mono_off) <= self.now_us);
                if due {
                    self.input(i, Input::Tick);
                }
            }
        }
    }

    /// Run in 500 ms steps until `pred` holds, at most `max_ms`. Returns whether it held.
    pub fn run_until(&mut self, max_ms: u64, mut pred: impl FnMut(&Sim) -> bool) -> bool {
        let mut spent = 0;
        while spent < max_ms {
            if pred(self) {
                return true;
            }
            self.run_for(500);
            spent += 500;
        }
        pred(self)
    }

    // ------------------------------------------------------------------
    // Scripted actions
    // ------------------------------------------------------------------

    /// Send a command to node `i`'s client and return the reply it produced.
    pub fn request(&mut self, i: usize, f: impl FnOnce(ReqId) -> LocalCommand) -> Reply {
        let req = self.next_req;
        self.next_req += 1;
        let mark = self.nodes[i].events.len();
        self.input(
            i,
            Input::Local {
                client: CLIENT,
                cmd: f(req),
            },
        );
        self.nodes[i].events[mark..]
            .iter()
            .find_map(|e| match e {
                LocalEvent::Reply { req: r, reply } if *r == req => Some(reply.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("node {i} gave no reply to request {req}"))
    }

    /// Send a command without a request id (e.g. `SendDatagram`).
    pub fn command(&mut self, i: usize, cmd: LocalCommand) {
        self.input(
            i,
            Input::Local {
                client: CLIENT,
                cmd,
            },
        );
    }

    pub fn subscribe(&mut self, i: usize, sub: u32, pattern: &str) {
        let p = pattern.to_string();
        let r = self.request(i, |req| LocalCommand::Subscribe {
            req,
            sub,
            pattern: p,
        });
        assert_eq!(r, Reply::Ok);
    }

    /// Node `i` joins via a token minted by node `via`.
    pub fn join(&mut self, i: usize, via: usize) {
        let Reply::Invite { token } = self.request(via, |req| LocalCommand::CreateInvite {
            req,
            expiry_ms: 0,
            relays: Vec::new(),
        }) else {
            panic!("node {via} could not invite");
        };
        self.join_with_token(i, &token);
    }

    pub fn join_with_token(&mut self, i: usize, token: &str) {
        let tok = InviteToken::decode(token).expect("token");
        let cfg = self.config(i);
        let now = self.now_of(i);
        self.nodes[i].room = Some(Room::join(cfg, tok, now));
        self.pump(i);
        self.attach(i);
    }

    /// Replay `recs` to node `to` as if peer `from` had sent them on their live
    /// connection (an attacker or a lagging peer re-sending an old record). Returns
    /// false when the two have no live link.
    pub fn replay_records(
        &mut self,
        to: usize,
        from: usize,
        recs: &[crate::record::Record],
    ) -> bool {
        let link = self
            .links
            .iter()
            .find(|(_, l)| l.up && ((l.a == to && l.b == from) || (l.a == from && l.b == to)))
            .map(|(c, _)| *c);
        let Some(conn) = link else {
            return false;
        };
        let bytes = Frame::Records(recs.iter().map(|r| r.bytes.clone()).collect()).encode();
        let at = self.now_us + self.latency_us();
        self.schedule(
            at,
            Ev::Frame {
                node: to,
                conn,
                lane: Lane::Control,
                bytes,
            },
        );
        true
    }

    /// Cut the network into `groups`; nodes not listed are together in one more group.
    pub fn partition(&mut self, groups: &[&[usize]]) {
        let n = self.nodes.len();
        self.group = vec![0; n];
        for (gi, g) in groups.iter().enumerate() {
            for &i in g.iter() {
                self.group[i] = gi as u32 + 1;
            }
        }
        let cut: Vec<u64> = self
            .links
            .iter()
            .filter(|(_, l)| l.up && self.group[l.a] != self.group[l.b])
            .map(|(c, _)| *c)
            .collect();
        for c in cut {
            let (a, b) = {
                let l = self.links.get_mut(&c).unwrap();
                l.up = false;
                (l.a, l.b)
            };
            for node in [a, b] {
                let at = self.now_us + self.cfg.detect_ms * 1000 + self.rng.below(1_000_000);
                self.schedule(at, Ev::Down { node, conn: c });
            }
        }
    }

    pub fn heal(&mut self) {
        self.group = vec![0; self.nodes.len()];
    }

    /// Stop node `i`. A silent stop leaves its peers to notice only after
    /// `detect_ms` (a crash); otherwise they see the disconnect after one latency.
    pub fn stop(&mut self, i: usize, silent: bool) {
        self.nodes[i].up = false;
        let mine: Vec<u64> = self
            .links
            .iter()
            .filter(|(_, l)| l.up && (l.a == i || l.b == i))
            .map(|(c, _)| *c)
            .collect();
        for c in mine {
            let other = {
                let l = self.links.get_mut(&c).unwrap();
                l.up = false;
                if l.a == i {
                    l.b
                } else {
                    l.a
                }
            };
            let d = if silent {
                self.cfg.detect_ms * 1000
            } else {
                self.latency_us()
            };
            self.schedule(
                self.now_us + d,
                Ev::Down {
                    node: other,
                    conn: c,
                },
            );
        }
    }

    /// Restart node `i` from its last persisted state with `member_incarnation + 1`.
    pub fn restart(&mut self, i: usize) {
        self.nodes[i].incarnation += 1;
        let persisted = self.nodes[i].persisted.clone().expect("nothing persisted");
        let cfg = self.config(i);
        let now = self.now_of(i);
        self.nodes[i].room = Some(Room::resume(cfg, persisted, now).expect("resume"));
        self.nodes[i].up = true;
        self.nodes[i].updates.clear();
        self.pump(i);
        self.attach(i);
    }

    // ------------------------------------------------------------------
    // Observation
    // ------------------------------------------------------------------

    /// Take (and clear) node `i`'s collected events.
    pub fn take_events(&mut self, i: usize) -> Vec<LocalEvent> {
        std::mem::take(&mut self.nodes[i].events)
    }

    pub fn visible(&self, i: usize) -> &BTreeMap<String, Vis> {
        self.room(i).visible_state()
    }

    pub fn status(&self, i: usize) -> Status {
        self.room(i).status(self.now_of(i))
    }

    /// Nodes that are running and hold a room with a grant set.
    pub fn live_nodes(&self) -> Vec<usize> {
        (0..self.nodes.len())
            .filter(|&i| {
                let n = &self.nodes[i];
                n.up && n
                    .room
                    .as_ref()
                    .is_some_and(|r| r.grants().is_some() && !r.is_removed())
            })
            .collect()
    }

    /// The convergence oracle: every live node holds the same grant records, the same
    /// visible durable state, the same lifecycle and the same coordinator.
    pub fn converged(&self) -> Result<(), String> {
        let live = self.live_nodes();
        let Some(&first) = live.first() else {
            return Ok(());
        };
        let ids = |i: usize| -> Vec<_> {
            let mut v: Vec<_> = self
                .room(i)
                .grants()
                .unwrap()
                .records()
                .map(|r| r.id)
                .collect();
            v.sort();
            v
        };
        let durable = |i: usize| -> Vec<(String, _, Vec<u8>)> {
            self.visible(i)
                .iter()
                .filter(|(_, v)| v.class == crate::staterec::Class::Durable)
                .map(|(p, v)| (p.clone(), v.version, v.value.clone()))
                .collect()
        };
        let coord = |i: usize| self.room(i).coordinator();
        let life = |i: usize| -> Lifecycle { self.room(i).grants().unwrap().lifecycle() };
        for &i in &live[1..] {
            if ids(i) != ids(first) {
                return Err(format!("grant sets differ: n{first} vs n{i}"));
            }
            if life(i) != life(first) {
                return Err(format!("lifecycle differs: n{first} vs n{i}"));
            }
            if durable(i) != durable(first) {
                return Err(format!(
                    "visible durable state differs: n{first} {:?} vs n{i} {:?}",
                    durable(first)
                        .iter()
                        .map(|d| (&d.0, d.1))
                        .collect::<Vec<_>>(),
                    durable(i).iter().map(|d| (&d.0, d.1)).collect::<Vec<_>>(),
                ));
            }
            if coord(i) != coord(first) {
                return Err(format!(
                    "coordinator differs: n{first} {:?} vs n{i} {:?}",
                    coord(first),
                    coord(i)
                ));
            }
        }
        if matches!(life(first), Lifecycle::Open(_)) && coord(first).is_none() {
            return Err("open room without a coordinator".into());
        }
        Ok(())
    }
}
