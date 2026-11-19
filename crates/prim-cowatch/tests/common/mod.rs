//! Test harness: N cowatch machines, each with a simulated player and a skewed clock,
//! attached to the real `room_core::sim::Sim` (real signed state, real coordinator
//! selection, real message acks). Only the media player and the per-machine clocks are
//! models.
#![allow(dead_code)]

use std::collections::BTreeMap;

use prim_cowatch::wire::*;
use prim_cowatch::*;
use room_core::api::{
    CoordinatorEvent, ErrorCode, LocalCommand, LocalEvent, MemberEvent, MessageStatus, Reply, To,
    Update,
};
use room_core::frame::AckStatus;
use room_core::ids::Key as RKey;
use room_core::sim::{NetConfig, Sim};
use room_core::staterec::{Class, Lifetime, Scope};

pub const STEP_MS: u64 = 50;

// --------------------------------------------------------------------------------
// Simulated player (the model: position is truth, reports are what mpv would say)
// --------------------------------------------------------------------------------

pub struct Player {
    pub token: u32,
    pub state: PlayerState,
    pub pos: f64,
    pub paused: bool,
    pub speed: f64,
    pub duration: f64,
    pub media_ppm: f64,
    pub load_ms: u64,
    pub seek_ms: u64,
    load_done: Option<u64>,
    seek_done: Option<(u64, f64)>,
    last_us: u64,
    pub has_src: bool,
    /// Drop the stream (early end of file) when the position first reaches this.
    pub drop_at: Option<f64>,
    /// Fail the next N loads immediately (server refusing).
    pub fail_loads: u32,
    pub loads: u32,
    pub seeks: u32,
    pub swallow_seeks: bool,
}

impl Player {
    pub fn new(duration: f64, media_ppm: f64) -> Self {
        Self {
            token: 0,
            state: PlayerState::Loading,
            pos: 0.0,
            paused: true,
            speed: 1.0,
            duration,
            media_ppm,
            load_ms: 400,
            seek_ms: 150,
            load_done: None,
            seek_done: None,
            last_us: 0,
            has_src: false,
            drop_at: None,
            fail_loads: 0,
            loads: 0,
            seeks: 0,
            swallow_seeks: false,
        }
    }

    pub fn advance(&mut self, now_us: u64) {
        let dt = now_us.saturating_sub(self.last_us) as f64 / 1e6;
        self.last_us = now_us;
        if let Some(t) = self.load_done {
            if now_us >= t {
                self.load_done = None;
                self.state = PlayerState::Ready;
                self.pos = 0.0;
                self.paused = true;
            }
        }
        if let Some((t, to)) = self.seek_done {
            if now_us >= t {
                self.seek_done = None;
                self.pos = to;
                self.state = PlayerState::Ready;
            }
        }
        if self.state == PlayerState::Ready && !self.paused {
            self.pos += dt * self.speed * (1.0 + self.media_ppm * 1e-6);
            if let Some(d) = self.drop_at {
                if self.pos >= d {
                    self.pos = d;
                    self.drop_at = None;
                    self.state = PlayerState::Ended;
                }
            }
            if self.pos >= self.duration {
                self.pos = self.duration;
                self.state = PlayerState::Ended;
            }
        }
    }

    pub fn apply(&mut self, now_us: u64, c: &PlayerCmd) {
        match c {
            PlayerCmd::Load { token, .. } => {
                self.token = *token;
                self.loads += 1;
                self.has_src = true;
                self.seek_done = None;
                self.speed = 1.0;
                if self.fail_loads > 0 {
                    self.fail_loads -= 1;
                    self.state = PlayerState::Error;
                    self.load_done = None;
                } else {
                    self.state = PlayerState::Loading;
                    self.load_done = Some(now_us + self.load_ms * 1000);
                }
            }
            PlayerCmd::Seek(to) => {
                self.seeks += 1;
                if self.swallow_seeks || self.state == PlayerState::Loading {
                    return;
                }
                let to = to.clamp(0.0, self.duration);
                self.state = PlayerState::Seeking;
                self.seek_done = Some((now_us + self.seek_ms * 1000, to));
            }
            PlayerCmd::SetPaused(p) => self.paused = *p,
            PlayerCmd::SetSpeed(s) => self.speed = *s,
            PlayerCmd::Stop => {
                self.has_src = false;
                self.state = PlayerState::Loading;
            }
        }
    }

    pub fn report(&self) -> PlayerReport {
        PlayerReport {
            token: self.token,
            state: self.state,
            position_s: self.pos,
            duration_s: if self.state == PlayerState::Loading {
                0.0
            } else {
                self.duration
            },
            paused: self.paused,
        }
    }

    pub fn playing_ready(&self) -> bool {
        self.has_src && self.state == PlayerState::Ready
    }
}

// --------------------------------------------------------------------------------
// One participant
// --------------------------------------------------------------------------------

pub struct Node {
    pub m: Cowatch,
    pub p: Player,
    pub ppm: f64,
    pub off_us: i64,
    pub alive: bool,
    ev_i: usize,
    up_i: BTreeMap<u32, usize>,
    msg_tokens: BTreeMap<u64, u64>,
    pub notices: Vec<(u64, Notice)>,
    pub send_log: Vec<(u64, SendOutcome)>,
    /// Sim time (µs) at which the player first reported Ready, per start.
    pub first_ready_us: Option<u64>,
    pub started_us: u64,
}

pub struct World {
    pub sim: Sim,
    pub nodes: Vec<Option<Node>>,
    pub faults: Faults,
}

pub fn net(loss_pct: u64, jitter_ms: u64) -> NetConfig {
    NetConfig {
        dgram_loss_pct: loss_pct,
        jitter_ms,
        ..NetConfig::default()
    }
}

impl World {
    /// `n` daemons, all joined and connected; no machines yet.
    pub fn new(seed: u64, n: usize, cfg: NetConfig) -> World {
        let mut sim = Sim::with_config(seed, n, cfg);
        // Node 0 founds; build the room like `Sim::bootstrap`, with the given net config.
        for i in 1..n {
            sim.join(i, 0);
            sim.run_for(2_000);
        }
        sim.run_for(15_000);
        World {
            sim,
            nodes: (0..n).map(|_| None).collect(),
            faults: Faults::default(),
        }
    }

    pub fn clock(&self, i: usize) -> u64 {
        let n = self.nodes[i].as_ref().unwrap();
        ((self.sim.now_us as f64 * (1.0 + n.ppm * 1e-6)) as i64 + n.off_us).max(0) as u64
    }

    fn clock_of(&self, now_us: u64, ppm: f64, off_us: i64) -> u64 {
        ((now_us as f64 * (1.0 + ppm * 1e-6)) as i64 + off_us).max(0) as u64
    }

    /// Start a player on daemon `i`.
    pub fn start(&mut self, i: usize, duration: f64, ppm: f64, off_us: i64, media_ppm: f64) {
        let now = self.sim.now_us;
        let me = self.sim.key(i).0;
        let mut cfg = prim_cowatch::Config::new(me, 1_800_000_000_000 + now / 1000);
        cfg.faults = self.faults;
        let clk = self.clock_of(now, ppm, off_us);
        let (m, actions) = Cowatch::new(cfg, clk);
        self.nodes[i] = Some(Node {
            m,
            p: Player::new(duration, media_ppm),
            ppm,
            off_us,
            alive: true,
            ev_i: self.sim.nodes[i].events.len(),
            up_i: BTreeMap::new(),
            msg_tokens: BTreeMap::new(),
            notices: Vec::new(),
            send_log: Vec::new(),
            first_ready_us: None,
            started_us: now,
        });
        for ch in [CH_CONTROL, CH_PROBE] {
            let r = self
                .sim
                .request(i, |req| LocalCommand::RegisterChannel { req, channel: ch });
            assert_eq!(r, Reply::Ok);
        }
        for (sub, pat) in [
            (1u32, "room/playback"),
            (2, "member/*/playback"),
            (3, "member/*/cowatch"),
        ] {
            self.sim.subscribe(i, sub, pat);
        }
        let mut q = Vec::new();
        if let Some((e, h)) = self.sim.status(i).coordinator {
            q.push(Event::Coordinator(Some(CoordView {
                generation: e.generation,
                term: e.term,
                holder: h.0,
                state: CoordState::Live,
            })));
        }
        self.exec(i, actions, &mut q);
        self.drain(i, q);
        self.collect(i);
    }

    /// Player process dies; the daemon stays up with its channels unregistered and the
    /// capability record still published.
    pub fn crash_player(&mut self, i: usize) {
        for ch in [CH_CONTROL, CH_PROBE] {
            self.sim
                .command(i, LocalCommand::UnregisterChannel { channel: ch });
        }
        self.nodes[i].as_mut().unwrap().alive = false;
    }

    /// Daemon (and its player) go away. `silent`: crash without a goodbye.
    pub fn kill(&mut self, i: usize, silent: bool) {
        if !silent {
            let r = self.sim.request(i, |req| LocalCommand::Leave { req });
            let _ = r;
            self.sim.run_for(300);
        }
        self.sim.stop(i, silent);
        self.nodes[i].as_mut().unwrap().alive = false;
    }

    pub fn n(&self, i: usize) -> &Node {
        self.nodes[i].as_ref().unwrap()
    }
    pub fn nm(&mut self, i: usize) -> &mut Node {
        self.nodes[i].as_mut().unwrap()
    }

    pub fn command(&mut self, i: usize, c: UserCommand) {
        let clk = self.clock(i);
        let a = self.nm(i).m.handle(clk, Event::Command(c));
        let mut q = Vec::new();
        self.exec(i, a, &mut q);
        self.drain(i, q);
    }

    fn drain(&mut self, i: usize, mut q: Vec<Event>) {
        while !q.is_empty() {
            let evs = std::mem::take(&mut q);
            for ev in evs {
                let clk = self.clock(i);
                let a = self.nm(i).m.handle(clk, ev);
                self.exec(i, a, &mut q);
            }
        }
    }

    fn exec(&mut self, i: usize, actions: Vec<Action>, q: &mut Vec<Event>) {
        let sim_now = self.sim.now_us;
        for a in actions {
            match a {
                Action::Write {
                    token,
                    scope,
                    name,
                    value,
                } => {
                    let scope = match scope {
                        WriteScope::Room => Scope::Room,
                        WriteScope::Member => Scope::Member,
                    };
                    let name = name.to_string();
                    let r = self.sim.request(i, |req| LocalCommand::Write {
                        req,
                        scope,
                        name,
                        class: Class::Durable,
                        lifetime: Lifetime::PresenceBound,
                        value,
                    });
                    q.push(Event::WriteResult {
                        token,
                        ok: r == Reply::Ok,
                    });
                }
                Action::Send { token, to, payload } => {
                    let to = match to {
                        Dest::Coordinator => To::Coordinator,
                        Dest::Member(k) => To::Member(RKey(k)),
                    };
                    let r = self.sim.request(i, |req| LocalCommand::Send {
                        req,
                        to,
                        channel: CH_CONTROL,
                        reliable: true,
                        payload,
                    });
                    match r {
                        Reply::Sending { msg_id, .. } => {
                            self.nm(i).msg_tokens.insert(msg_id, token);
                        }
                        Reply::Error(ErrorCode::NoCoordinator) => {
                            self.nm(i)
                                .send_log
                                .push((sim_now, SendOutcome::NoCoordinator));
                            q.push(Event::SendResult {
                                token,
                                outcome: SendOutcome::NoCoordinator,
                            })
                        }
                        Reply::Error(ErrorCode::Offline) => {
                            self.nm(i).send_log.push((sim_now, SendOutcome::Offline));
                            q.push(Event::SendResult {
                                token,
                                outcome: SendOutcome::Offline,
                            })
                        }
                        _ => q.push(Event::SendResult {
                            token,
                            outcome: SendOutcome::Other,
                        }),
                    }
                }
                Action::Datagram { to, payload } => {
                    self.sim.command(
                        i,
                        LocalCommand::SendDatagram {
                            to: To::Member(RKey(to)),
                            channel: CH_PROBE,
                            payload,
                            max_age_ms: 500,
                        },
                    );
                }
                Action::Player(c) => self.nm(i).p.apply(sim_now, &c),
                Action::Notice(nt) => self.nm(i).notices.push((sim_now, nt)),
            }
        }
    }

    /// Translate the daemon's new events and subscription updates for node `i`.
    fn collect(&mut self, i: usize) {
        let mut q = Vec::new();
        let sim_now = self.sim.now_us;
        let evs: Vec<LocalEvent> = {
            let n = self.nodes[i].as_mut().unwrap();
            let all = &self.sim.nodes[i].events;
            let new = all[n.ev_i..].to_vec();
            n.ev_i = all.len();
            new
        };
        for e in evs {
            match e {
                LocalEvent::Message {
                    from,
                    channel,
                    payload,
                } if channel == CH_CONTROL => q.push(Event::Message {
                    from: from.0,
                    payload,
                }),
                LocalEvent::Datagram {
                    from,
                    channel,
                    payload,
                    ..
                } if channel == CH_PROBE => q.push(Event::Datagram {
                    from: from.0,
                    payload,
                }),
                LocalEvent::MessageResult { msg_id, status, .. } => {
                    let n = self.nodes[i].as_mut().unwrap();
                    if let Some(token) = n.msg_tokens.remove(&msg_id) {
                        let outcome = match status {
                            MessageStatus::Acked(AckStatus::Delivered) => SendOutcome::Delivered,
                            MessageStatus::Acked(AckStatus::NoChannel) => SendOutcome::NoChannel,
                            MessageStatus::Acked(AckStatus::NotMember) => SendOutcome::NotMember,
                            MessageStatus::Acked(AckStatus::TooLarge) => SendOutcome::TooLarge,
                            MessageStatus::Lost => SendOutcome::Lost,
                            MessageStatus::Offline => SendOutcome::Offline,
                        };
                        n.send_log.push((sim_now, outcome));
                        q.push(Event::SendResult { token, outcome });
                    }
                }
                LocalEvent::Member { key, event } => {
                    let state = match event {
                        MemberEvent::Joined | MemberEvent::Online => MemberState::Online,
                        MemberEvent::Offline => MemberState::Offline,
                        MemberEvent::Unresponsive => MemberState::Unresponsive,
                        MemberEvent::Removed => MemberState::Removed,
                    };
                    q.push(Event::Member { key: key.0, state });
                }
                LocalEvent::Coordinator(ev) => {
                    let (epoch, holder, state) = match ev {
                        CoordinatorEvent::Changed { epoch, holder } => {
                            (epoch, holder, CoordState::Live)
                        }
                        CoordinatorEvent::Left { epoch, holder } => {
                            (epoch, holder, CoordState::Left)
                        }
                        CoordinatorEvent::Unresponsive { epoch, holder } => {
                            (epoch, holder, CoordState::Unresponsive)
                        }
                    };
                    q.push(Event::Coordinator(Some(CoordView {
                        generation: epoch.generation,
                        term: epoch.term,
                        holder: holder.0,
                        state,
                    })));
                }
                _ => {}
            }
        }
        for sub in [1u32, 2, 3] {
            let ups: Vec<Update> = {
                let n = self.nodes[i].as_mut().unwrap();
                let all = self.sim.nodes[i]
                    .updates
                    .get(&sub)
                    .cloned()
                    .unwrap_or_default();
                let from = n.up_i.get(&sub).copied().unwrap_or(0);
                n.up_i.insert(sub, all.len());
                all[from..].to_vec()
            };
            for u in ups {
                match u {
                    Update::Reset => q.push(if sub == 3 {
                        Event::CapsReset
                    } else {
                        Event::PlaybackReset { room: sub == 1 }
                    }),
                    Update::Value { writer, value, .. } => {
                        if sub == 3 {
                            if value.is_empty() {
                                q.push(Event::CapRemoved { writer: writer.0 })
                            } else {
                                q.push(Event::Cap {
                                    writer: writer.0,
                                    value,
                                })
                            }
                        } else if value.is_empty() {
                            q.push(Event::PlaybackRemoved {
                                room: sub == 1,
                                writer: Some(writer.0),
                            })
                        } else {
                            q.push(Event::Playback {
                                room: sub == 1,
                                writer: writer.0,
                                value,
                            })
                        }
                    }
                    Update::Removed { path } => {
                        let w = path.split('/').nth(1).and_then(RKey::from_hex).map(|k| k.0);
                        if sub == 3 {
                            if let Some(w) = w {
                                q.push(Event::CapRemoved { writer: w })
                            }
                        } else {
                            q.push(Event::PlaybackRemoved {
                                room: sub == 1,
                                writer: if sub == 1 { None } else { w },
                            })
                        }
                    }
                    Update::SnapshotDone => {}
                }
            }
        }
        self.drain(i, q);
    }

    /// Advance by one step: daemon time, then each live machine (events, player report, tick).
    pub fn step(&mut self) {
        self.sim.run_for(STEP_MS);
        let now = self.sim.now_us;
        for i in 0..self.nodes.len() {
            if !self.nodes[i].as_ref().is_some_and(|n| n.alive) {
                continue;
            }
            self.collect(i);
            let n = self.nodes[i].as_mut().unwrap();
            n.p.advance(now);
            if n.p.playing_ready() && n.first_ready_us.is_none() {
                n.first_ready_us = Some(now);
            }
            let rep = n.p.report();
            let has_src = n.p.has_src;
            let mut q = Vec::new();
            if has_src {
                q.push(Event::Player(rep));
            }
            q.push(Event::Tick);
            self.drain(i, q);
        }
    }

    pub fn run(&mut self, ms: u64) {
        for _ in 0..ms / STEP_MS {
            self.step();
        }
    }

    /// Run until `pred` holds, at most `max_ms`; returns the sim ms it took, or None.
    pub fn run_until(&mut self, max_ms: u64, mut pred: impl FnMut(&World) -> bool) -> Option<u64> {
        let mut t = 0;
        while t <= max_ms {
            if pred(self) {
                return Some(t);
            }
            self.step();
            t += STEP_MS;
        }
        None
    }

    // ----- oracle helpers ------------------------------------------------------------

    pub fn live_players(&self) -> Vec<usize> {
        (0..self.nodes.len())
            .filter(|&i| {
                self.nodes[i]
                    .as_ref()
                    .is_some_and(|n| n.alive && n.p.playing_ready())
            })
            .collect()
    }

    /// Largest position difference among ready players of `idxs` (seconds).
    pub fn skew(&self, idxs: &[usize]) -> f64 {
        let ps: Vec<f64> = idxs
            .iter()
            .filter(|&&i| self.n(i).p.playing_ready())
            .map(|&i| self.n(i).p.pos)
            .collect();
        if ps.len() < 2 {
            return 0.0;
        }
        ps.iter().cloned().fold(f64::MIN, f64::max) - ps.iter().cloned().fold(f64::MAX, f64::min)
    }

    pub fn all_paused(&self, idxs: &[usize]) -> bool {
        idxs.iter()
            .all(|&i| self.n(i).p.playing_ready() && self.n(i).p.paused)
    }

    pub fn all_playing(&self, idxs: &[usize]) -> bool {
        idxs.iter()
            .all(|&i| self.n(i).p.playing_ready() && !self.n(i).p.paused)
    }
}

pub fn url(s: &str) -> UserCommand {
    UserCommand::Load {
        kind: SourceKind::Url,
        source: s.as_bytes().to_vec(),
    }
}
