//! The state machine. See the crate docs and the P1 report for the design.
//!
//! Roles
//! - **Authority**: the one member whose `Intent` record counts. Normally the room
//!   coordinator, writing `room/playback` ([`Mode::Coordinator`]), provided it runs a
//!   player (advertised by `member/<k>/cowatch`). If the coordinator has no player or is
//!   unreachable, the lowest-ranked member that runs one acts instead and writes
//!   `member/<me>/playback` ([`Mode::Acting`]). Every member derives the same answer
//!   from the same records, so there is no election traffic.
//! - **Follower**: everyone else, and also the authority's own player: every player,
//!   the authority's included, follows the record through the same correction loop.

use std::collections::BTreeMap;

use crate::clock::Estimator;
use crate::correct::{correct, target_position, Inputs, Params};
use crate::wire::*;

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberState {
    Online,
    Offline,
    Unresponsive,
    Removed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoordState {
    Live,
    Left,
    Unresponsive,
}

/// The daemon's coordinator, as `CoordinatorEvent`/`Status` report it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoordView {
    pub generation: u32,
    pub term: u32,
    pub holder: Key,
    pub state: CoordState,
}

/// What came back for a message we sent (`MessageResult`, or the `Reply` to `Send`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendOutcome {
    Delivered,
    NoChannel,
    NotMember,
    TooLarge,
    Lost,
    Offline,
    NoCoordinator,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerState {
    Loading,
    Ready,
    Seeking,
    Buffering,
    /// End of file/stream, clean or not. The machine decides which.
    Ended,
    Error,
}

/// Report the host sends on every tick (at least every 250 ms).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerReport {
    /// The token of the last `PlayerCmd::Load` the player has acted on.
    pub token: u32,
    pub state: PlayerState,
    pub position_s: f64,
    /// 0 when unknown (live, or not yet probed).
    pub duration_s: f64,
    pub paused: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum UserCommand {
    Load { kind: SourceKind, source: Vec<u8> },
    Play,
    Pause,
    Toggle,
    SeekTo(f64),
    SeekBy(f64),
}

#[derive(Clone, Debug)]
pub enum Event {
    Tick,
    Coordinator(Option<CoordView>),
    Member {
        key: Key,
        state: MemberState,
    },
    /// A value of `room/playback` (`room: true`) or `member/<writer>/playback`.
    Playback {
        room: bool,
        writer: Key,
        value: Vec<u8>,
    },
    PlaybackRemoved {
        room: bool,
        writer: Option<Key>,
    },
    Cap {
        writer: Key,
        value: Vec<u8>,
    },
    CapRemoved {
        writer: Key,
    },
    /// A playback subscription restarted (`room/playback` if `room`, else
    /// `member/*/playback`); a full snapshot of values follows.
    PlaybackReset {
        room: bool,
    },
    /// The `member/*/cowatch` subscription restarted.
    CapsReset,
    /// The first snapshot of all three subscriptions (`room/playback`,
    /// `member/*/playback`, `member/*/cowatch`) has been delivered. Until then user
    /// commands are held: this process may be the restart of an earlier one with the same
    /// member key, whose records are still in the room and are not in our view yet. Send
    /// it once per process, after the last `SnapshotDone`. A host that never sends it
    /// has its commands released after [`Config::sync_wait_ms`].
    Synced,
    Message {
        from: Key,
        payload: Vec<u8>,
    },
    Datagram {
        from: Key,
        payload: Vec<u8>,
    },
    SendResult {
        token: u64,
        outcome: SendOutcome,
    },
    WriteResult {
        token: u64,
        ok: bool,
    },
    Player(PlayerReport),
    Command(UserCommand),
}

// ---------------------------------------------------------------------------
// Outputs
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteScope {
    Room,
    Member,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dest {
    Coordinator,
    Member(Key),
}

#[derive(Clone, Debug, PartialEq)]
pub enum PlayerCmd {
    /// Load paused, then report with this token. `start_s` is a hint; the machine
    /// seeks to the room position once the report says the file is loaded.
    Load {
        token: u32,
        kind: SourceKind,
        source: Vec<u8>,
        start_s: f64,
    },
    Seek(f64),
    SetPaused(bool),
    SetSpeed(f64),
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    /// Clean end of media.
    Finished,
    /// The stream ended early; reloading `attempt` of `of` in `in_ms`.
    Reloading { attempt: u32, of: u32, in_ms: u64 },
    /// Reloads exhausted.
    Failed,
    /// A request ran out of time without a reachable authority.
    ControlUnavailable,
    /// Nobody in the room runs a player.
    NoAuthority,
    /// An authority exists again.
    AuthorityFound,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Write a durable record. `name` is [`NAME_PLAYBACK`] or [`NAME_CAP`]; an empty
    /// value deletes. Answer with `Event::WriteResult { token }`.
    Write {
        token: u64,
        scope: WriteScope,
        name: &'static str,
        value: Vec<u8>,
    },
    /// Reliable app message on [`CH_CONTROL`]. Answer with `Event::SendResult { token }`.
    Send {
        token: u64,
        to: Dest,
        payload: Vec<u8>,
    },
    /// Unreliable app datagram on [`CH_PROBE`] to a member.
    Datagram {
        to: Key,
        payload: Vec<u8>,
    },
    Player(PlayerCmd),
    Notice(Notice),
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Test-only fault injection. Clients leave this at its default.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Faults {
    /// Estimates and probes ignore the (generation, term, holder) key.
    pub probe_ignores_epoch: bool,
    /// Every end of stream is treated as a dropped connection.
    pub always_reload: bool,
    /// Never tell the player to seek or change speed.
    pub no_correction: bool,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub me: Key,
    /// Unix ms when this player started; ranks acting leaders (lowest wins).
    pub since_ms: u64,
    pub params: Params,
    /// An end of stream this far before the known duration is an early EOF.
    pub early_eof_slack_s: f64,
    pub reload_backoff_ms: Vec<u64>,
    pub request_ttl_ms: u64,
    /// How long user commands wait for [`Event::Synced`].
    pub sync_wait_ms: u64,
    pub probe_interval_ms: u64,
    pub correct_interval_ms: u64,
    pub block_ms: u64,
    /// A drift above this for `persist_after_ms` is corrected by a seek, not by speed.
    pub persist_drift_s: f64,
    pub persist_after_ms: u64,
    pub faults: Faults,
}

impl Config {
    pub fn new(me: Key, since_ms: u64) -> Self {
        Self {
            me,
            since_ms,
            params: Params::default(),
            early_eof_slack_s: 3.0,
            reload_backoff_ms: vec![500, 1_000, 2_000, 4_000, 8_000, 16_000],
            request_ttl_ms: 30_000,
            sync_wait_ms: 3_000,
            probe_interval_ms: 1_000,
            correct_interval_ms: 250,
            block_ms: 30_000,
            persist_drift_s: 0.25,
            persist_after_ms: 2_000,
            faults: Faults::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Coordinator,
    Acting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Auth {
    pub key: AuthKey,
    pub mode: Mode,
}

/// Snapshot for UI and tests.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub authority: Option<Auth>,
    pub is_authority: bool,
    pub rev: Option<u32>,
    pub drift_s: Option<f64>,
    pub clock_ready: bool,
    pub rtt_ms: Option<f64>,
    /// Estimated clock of the record's writer minus ours, microseconds.
    pub clock_offset_us: Option<i64>,
    pub loaded: bool,
    pub finished: bool,
    pub reload_attempts: u32,
    pub failed: bool,
    pub request_pending: bool,
}

// ---------------------------------------------------------------------------
// The machine
// ---------------------------------------------------------------------------

struct Pending {
    body: Body,
    req_id: u32,
    deadline_us: u64,
    next_try_us: u64,
    attempt: usize,
}

struct WriteMeta {
    scope: WriteScope,
    prev: Option<(Key, Intent)>,
    rev: u32,
    body: Body,
}

struct Cap1 {
    since_ms: u64,
}

const REQ_BACKOFF_MS: [u64; 5] = [250, 500, 1_000, 2_000, 2_000];

pub struct Cowatch {
    cfg: Config,
    est: Estimator,

    coord: Option<CoordView>,
    members: BTreeMap<Key, MemberState>,
    caps: BTreeMap<Key, Cap1>,
    blocked: BTreeMap<Key, (u64, u32)>,
    fails: BTreeMap<Key, u32>,
    ghost: Option<(Key, Intent)>,
    /// Retest pings in flight to blocked members: member -> probe id.
    retests: BTreeMap<Key, u32>,
    next_retest_id: u32,
    room_rec: Option<(Key, Intent)>,
    acting: BTreeMap<Key, Intent>,
    last_written_rev: u32,

    auth: Option<Auth>,
    carry: Option<(f64, bool)>,

    pending: Option<Pending>,
    next_req: u32,
    next_token: u64,
    in_flight: BTreeMap<u64, u32>,
    writes: BTreeMap<u64, WriteMeta>,
    last_from: BTreeMap<Key, (u32, ReplyStatus, u32)>,

    next_probe_us: u64,
    burst_left: u8,

    // player
    player: Option<PlayerReport>,
    player_src: Option<(SourceKind, Vec<u8>)>,
    load_token: u32,
    loaded: bool,
    loaded_at_us: u64,
    awaiting_seek: bool,
    was_buffering: bool,
    cmd_speed: Option<f64>,
    cmd_paused: Option<bool>,
    seek_hold_until_us: u64,
    last_correct_us: u64,
    drift_s: Option<f64>,
    seen_rev: Option<u32>,
    finished: bool,
    reload_attempts: u32,
    reload_at: Option<(u64, f64)>,
    failed: bool,
    notice_no_auth: bool,
    force_next: bool,
    seek_started_us: Option<u64>,
    seen_seeking: bool,
    seek_lead_s: f64,
    drift_over_since_us: Option<u64>,
    now_us: u64,
    probe_since_us: u64,
    adopt_since: Option<u64>,
    started_us: u64,
    synced: bool,
    /// Commands from the person that arrived before [`Event::Synced`].
    held: Vec<UserCommand>,
    /// The latest record this process wrote, to restate it if a predecessor's record
    /// (same writer key, higher revision) overwrites it in our view.
    last_own: Option<(WriteScope, Intent)>,
}

fn is_live_url(s: &[u8]) -> bool {
    [&b"rtsp://"[..], b"rtsps://", b"rtmp://", b"rtmps://"]
        .iter()
        .any(|p| s.starts_with(p))
}

impl Cowatch {
    /// Create the machine. The returned actions advertise this player.
    pub fn new(cfg: Config, now_us: u64) -> (Self, Vec<Action>) {
        let mut est = Estimator::default();
        est.ignore_epoch = cfg.faults.probe_ignores_epoch;
        let mut caps = BTreeMap::new();
        caps.insert(
            cfg.me,
            Cap1 {
                since_ms: cfg.since_ms,
            },
        );
        let mut m = Self {
            est,
            coord: None,
            members: BTreeMap::new(),
            caps,
            blocked: BTreeMap::new(),
            fails: BTreeMap::new(),
            ghost: None,
            retests: BTreeMap::new(),
            next_retest_id: 0x8000_0000,
            room_rec: None,
            acting: BTreeMap::new(),
            last_written_rev: 0,
            auth: None,
            carry: None,
            pending: None,
            // Request ids must not repeat across restarts of the same member key: the
            // authority dedupes by (sender, id). Seed from the start time (10 ms units);
            // an id that still lands behind the authority's is answered `Stale` and
            // re-sent above it.
            next_req: ((cfg.since_ms / 10) as u32 & 0x7fff_ffff).max(1),
            next_token: 1,
            in_flight: BTreeMap::new(),
            writes: BTreeMap::new(),
            last_from: BTreeMap::new(),
            next_probe_us: now_us,
            burst_left: 0,
            player: None,
            player_src: None,
            load_token: 0,
            loaded: false,
            loaded_at_us: 0,
            awaiting_seek: false,
            was_buffering: false,
            cmd_speed: None,
            cmd_paused: None,
            seek_hold_until_us: 0,
            last_correct_us: 0,
            drift_s: None,
            seen_rev: None,
            finished: false,
            reload_attempts: 0,
            reload_at: None,
            failed: false,
            notice_no_auth: false,
            force_next: false,
            seek_started_us: None,
            seen_seeking: false,
            seek_lead_s: 0.0,
            drift_over_since_us: None,
            now_us,
            probe_since_us: now_us,
            adopt_since: None,
            started_us: now_us,
            synced: false,
            held: Vec::new(),
            last_own: None,
            cfg,
        };
        let mut out = Vec::new();
        let tok = m.token();
        out.push(Action::Write {
            token: tok,
            scope: WriteScope::Member,
            name: NAME_CAP,
            value: Cap {
                since_ms: m.cfg.since_ms,
            }
            .encode(),
        });
        m.reevaluate(now_us, &mut out);
        (m, out)
    }

    /// Retract this player's capability (clean exit).
    pub fn stop(&mut self) -> Vec<Action> {
        let token = self.token();
        vec![
            Action::Write {
                token,
                scope: WriteScope::Member,
                name: NAME_CAP,
                value: Vec::new(),
            },
            Action::Player(PlayerCmd::Stop),
        ]
    }

    fn token(&mut self) -> u64 {
        self.next_token += 1;
        self.next_token
    }

    pub fn view(&self, now_us: u64) -> View {
        View {
            authority: self.auth,
            is_authority: self.is_authority(),
            rev: self.effective().map(|(_, i)| i.rev),
            drift_s: self.drift_s,
            clock_ready: self.host_now(now_us).is_some(),
            rtt_ms: self.est.best_rtt_us(now_us).map(|r| r as f64 / 1000.0),
            clock_offset_us: self.est.offset_us(now_us),
            loaded: self.loaded,
            finished: self.finished,
            reload_attempts: self.reload_attempts,
            failed: self.failed,
            request_pending: self.pending.is_some(),
        }
    }

    pub fn is_authority(&self) -> bool {
        self.auth.is_some_and(|a| a.key.holder == self.cfg.me)
    }

    // ----- event entry -----------------------------------------------------

    pub fn handle(&mut self, now_us: u64, ev: Event) -> Vec<Action> {
        let mut out = Vec::new();
        self.now_us = now_us;
        match ev {
            Event::Tick => {
                if !self.synced && now_us.saturating_sub(self.started_us) >= self.cfg.sync_wait_ms * 1000 {
                    self.release_held(now_us, &mut out);
                }
            }
            Event::Synced => self.release_held(now_us, &mut out),
            Event::Coordinator(c) => {
                self.coord = c;
                self.reevaluate(now_us, &mut out);
            }
            Event::Member { key, state } => {
                self.members.insert(key, state);
                self.reevaluate(now_us, &mut out);
            }
            Event::Cap { writer, value } => {
                if let Ok(c) = Cap::decode(&value) {
                    self.caps.insert(
                        writer,
                        Cap1 {
                            since_ms: c.since_ms,
                        },
                    );
                    self.reevaluate(now_us, &mut out);
                }
            }
            Event::CapRemoved { writer } => {
                if writer != self.cfg.me {
                    self.caps.remove(&writer);
                    self.blocked.remove(&writer);
                    self.retests.remove(&writer);
                    self.reevaluate(now_us, &mut out);
                }
            }
            Event::PlaybackReset { room } => {
                if room {
                    self.room_rec = None;
                } else {
                    self.acting.clear();
                }
                self.reevaluate(now_us, &mut out);
            }
            Event::CapsReset => {
                let me = self.cfg.me;
                self.caps.retain(|k, _| *k == me);
                self.reevaluate(now_us, &mut out);
            }
            Event::Playback {
                room,
                writer,
                value,
            } => {
                if let Ok(i) = Intent::decode(&value) {
                    let predecessor = self.store_intent(room, writer, i);
                    self.reevaluate(now_us, &mut out);
                    if predecessor {
                        self.restate_own(now_us, &mut out);
                    }
                }
            }
            Event::PlaybackRemoved { room, writer } => {
                // The record in force may vanish before the daemon's member event reaches
                // us (presence-bound records go with their writer): keep it as a ghost so
                // the successor still has a timeline to continue and players keep going.
                let was_effective = self.effective().map(|(w, i)| (w, i.clone()));
                if let Some((w, i)) = was_effective {
                    let hit = if room {
                        self.room_rec.as_ref().is_some_and(|r| r.0 == w)
                    } else {
                        writer == Some(w)
                    };
                    if hit {
                        self.ghost = Some((w, i));
                    }
                }
                if room {
                    self.room_rec = None;
                } else if let Some(w) = writer {
                    self.acting.remove(&w);
                }
                self.reevaluate(now_us, &mut out);
            }
            Event::Message { from, payload } => self.on_message(now_us, from, &payload, &mut out),
            Event::Datagram { from, payload } => self.on_datagram(now_us, from, &payload, &mut out),
            Event::SendResult { token, outcome } => {
                self.on_send_result(now_us, token, outcome, &mut out)
            }
            Event::WriteResult { token, ok } => self.on_write_result(now_us, token, ok, &mut out),
            Event::Player(r) => self.on_player(now_us, r, &mut out),
            Event::Command(c) => {
                if self.synced {
                    self.on_command(now_us, c, &mut out);
                } else if self.held.len() < 32 {
                    self.held.push(c);
                }
            }
        }
        self.run(now_us, &mut out);
        out
    }

    // ----- record store ------------------------------------------------------

    /// Returns true when the record is by our own key with a revision this process never
    /// wrote (an earlier run's, still in the room) and it displaced a record we wrote.
    fn store_intent(&mut self, room: bool, writer: Key, i: Intent) -> bool {
        // A revision this high can only be hostile or corrupt; it would freeze the room.
        if i.rev >= 0x8000_0000 {
            return false;
        }
        self.ghost = None;
        let me = self.cfg.me;
        let mut displaced = false;
        if room {
            // The daemon only delivers room records valid for their epoch, so a foreign
            // writer's record replaces ours whatever its revision (we may be an
            // ex-coordinator whose partition-time record is now void). Only our own
            // echo is ordered against our latest.
            if let Some((w, old)) = &self.room_rec {
                if *w == me && writer == me {
                    if old.rev > i.rev {
                        return false;
                    }
                    displaced = i.rev > self.last_written_rev;
                }
            }
            self.room_rec = Some((writer, i));
        } else {
            if let Some(old) = self.acting.get(&writer) {
                if writer == me {
                    if old.rev > i.rev {
                        return false;
                    }
                    displaced = i.rev > self.last_written_rev;
                }
            }
            self.acting.insert(writer, i);
        }
        displaced
    }

    /// A predecessor run of this member key left a record above everything this process
    /// has written, and it replaced ours in our view. What the person did here is newer
    /// than that record: write it again above it, continuing its timeline.
    fn restate_own(&mut self, now_us: u64, out: &mut Vec<Action>) {
        let Some((scope, last)) = self.last_own.clone() else {
            return;
        };
        let Some(a) = self.auth else { return };
        let want = if a.mode == Mode::Coordinator {
            WriteScope::Room
        } else {
            WriteScope::Member
        };
        if a.key.holder != self.cfg.me || want != scope {
            return;
        }
        let elapsed = now_us.saturating_sub(last.ref_us) as f64 / 1e6;
        let mut pos = last.position_ms as f64 / 1000.0;
        if !last.paused {
            pos += elapsed * last.rate_milli as f64 / 1000.0;
        }
        let paused = last.paused;
        self.write_intent(now_us, a.mode, last, pos, paused, None, out);
    }

    fn release_held(&mut self, now_us: u64, out: &mut Vec<Action>) {
        if self.synced {
            return;
        }
        self.synced = true;
        for c in std::mem::take(&mut self.held) {
            self.on_command(now_us, c, out);
        }
    }

    /// Highest revision wins; at equal revisions the room record beats an acting one,
    /// then the lowest writer key.
    fn effective(&self) -> Option<(Key, &Intent)> {
        let room = self.room_rec.iter().map(|(w, i)| (*w, i, true));
        let acting = self.acting.iter().map(|(w, i)| (*w, i, false));
        room.chain(acting)
            .max_by_key(|(w, i, r)| (i.rev, *r, std::cmp::Reverse(*w)))
            .map(|(w, i, _)| (w, i))
            .or_else(|| self.ghost.as_ref().map(|(w, i)| (*w, i)))
    }

    fn max_rev(&self) -> u32 {
        let mut m = self.last_written_rev;
        if let Some((_, i)) = &self.room_rec {
            m = m.max(i.rev);
        }
        for i in self.acting.values() {
            m = m.max(i.rev);
        }
        m
    }

    /// The record every player follows: the effective one, whoever wrote it. A record of
    /// a member that has since gone keeps being projected with the last clock estimate
    /// until a new authority writes (or the estimate ages out and the player free-runs).
    fn usable(&self) -> Option<&Intent> {
        self.effective().map(|(_, i)| i)
    }

    /// Whose clock the effective record is in, as a probe key.
    fn clock_key(&self) -> Option<AuthKey> {
        let (w, _) = self.effective()?;
        if w == self.cfg.me {
            return None;
        }
        let (g, t) = self.coord.map_or((0, 0), |c| (c.generation, c.term));
        Some(AuthKey {
            generation: g,
            term: t,
            holder: w,
        })
    }

    /// Rekey the estimator to the writer of the effective record. A different
    /// (generation, term, holder) drops every sample and every probe in flight.
    fn sync_clock(&mut self, now_us: u64) {
        let key = self.clock_key();
        if self.est.set_key(key) {
            self.probe_since_us = now_us;
            self.burst_left = if key.is_some() { 3 } else { 0 };
            self.next_probe_us = now_us;
        }
    }

    // ----- authority ---------------------------------------------------------

    /// A member whose player stopped answering stays blocked until it answers again: the
    /// timestamp in the entry only says when to try it with the next retest ping.
    fn is_blocked(&self, k: &Key, _now_us: u64) -> bool {
        self.blocked.contains_key(k)
    }

    fn eligible(&self, k: &Key, now_us: u64) -> bool {
        if *k == self.cfg.me {
            return true;
        }
        self.caps.contains_key(k)
            && matches!(self.members.get(k), None | Some(MemberState::Online))
            && !self.is_blocked(k, now_us)
    }

    fn compute_auth(&self, now_us: u64) -> Option<Auth> {
        let (g, t) = self.coord.map_or((0, 0), |c| (c.generation, c.term));
        if let Some(c) = self.coord {
            if c.state == CoordState::Live
                && self.eligible(&c.holder, now_us)
                && self.caps.contains_key(&c.holder)
            {
                return Some(Auth {
                    key: AuthKey {
                        generation: g,
                        term: t,
                        holder: c.holder,
                    },
                    mode: Mode::Coordinator,
                });
            }
        }
        self.caps
            .iter()
            .filter(|(k, _)| self.eligible(k, now_us))
            .min_by_key(|(k, c)| (c.since_ms, **k))
            .map(|(k, _)| Auth {
                key: AuthKey {
                    generation: g,
                    term: t,
                    holder: *k,
                },
                mode: Mode::Acting,
            })
    }

    /// Recompute the authority; on a change rekey the clock and, if this member just
    /// became the authority, re-anchor the record to the room's current position.
    fn reevaluate(&mut self, now_us: u64, out: &mut Vec<Action>) {
        let new = self.compute_auth(now_us);
        if new == self.auth {
            return;
        }
        let old = self.auth;
        // Where the room is right now, from the estimate that may be about to be dropped.
        let carry = self.target(now_us).map(|(p, paused, _)| (p, paused));
        self.auth = new;
        self.fails.clear();
        if carry.is_some() {
            self.carry = carry;
        }
        match new {
            Some(a) if a.key.holder == self.cfg.me => {
                let changed_role =
                    old.is_none_or(|o| o.key.holder != self.cfg.me || o.mode != a.mode);
                if changed_role {
                    self.anchor(now_us, a.mode, out);
                }
            }
            _ => self.carry = None,
        }
        match (old, new) {
            (_, None) if !self.notice_no_auth => {
                self.notice_no_auth = true;
                out.push(Action::Notice(Notice::NoAuthority));
            }
            (_, Some(_)) if self.notice_no_auth => {
                self.notice_no_auth = false;
                out.push(Action::Notice(Notice::AuthorityFound));
            }
            _ => {}
        }
    }

    /// This member holds the authority and its own record may live in the wrong scope
    /// (it was acting and is now the coordinator, or the reverse): rewrite it.
    fn anchor(&mut self, now_us: u64, mode: Mode, out: &mut Vec<Action>) {
        let Some((w, base)) = self.effective().map(|(w, i)| (w, i.clone())) else {
            return;
        };
        if w != self.cfg.me || base.kind == SourceKind::None {
            return; // a foreign record is adopted in `adopt`
        }
        let (pos, paused) = self
            .carry
            .or_else(|| self.target(now_us).map(|(p, pa, _)| (p, pa)))
            .unwrap_or((base.position_ms as f64 / 1000.0, base.paused));
        self.write_intent(now_us, mode, base, pos, paused, None, out);
    }

    /// The authority is this member but the record in force is someone else's (the
    /// previous authority's, or one written before views converged): write a successor that
    /// continues the same timeline, so everyone re-keys onto this member. Needs a clock
    /// estimate to the old writer; without one after 3 s, hold the old position paused.
    fn adopt(&mut self, now_us: u64, out: &mut Vec<Action>) {
        let Some(a) = self.auth else { return };
        if a.key.holder != self.cfg.me {
            self.adopt_since = None;
            return;
        }
        let Some((w, base)) = self.effective().map(|(w, i)| (w, i.clone())) else {
            return;
        };
        if w == self.cfg.me || base.kind == SourceKind::None {
            self.adopt_since = None;
            return;
        }
        let (pos, paused) = match self.target(now_us) {
            Some((p, pa, _)) => (p, pa),
            None => {
                let since = *self.adopt_since.get_or_insert(now_us);
                if now_us.saturating_sub(since) < 3_000_000 {
                    return;
                }
                // No timeline to project: hold where our own player is, else at the record.
                let own = self.player.filter(|_| self.loaded).map(|p| p.position_s);
                (own.unwrap_or(base.position_ms as f64 / 1000.0), true)
            }
        };
        self.adopt_since = None;
        self.write_intent(now_us, a.mode, base, pos, paused, None, out);
    }

    // ----- writing intents (authority) -----------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn write_intent(
        &mut self,
        now_us: u64,
        mode: Mode,
        base: Intent,
        pos_s: f64,
        paused: bool,
        body: Option<Body>,
        out: &mut Vec<Action>,
    ) -> u32 {
        let rev = self.max_rev().saturating_add(1);
        self.last_written_rev = rev;
        let dur_ms = match (&self.player_src, self.player) {
            (Some((k, s)), Some(p))
                if *k == base.kind && *s == base.source && p.duration_s > 0.0 =>
            {
                (p.duration_s * 1000.0) as u64
            }
            _ => base.duration_ms,
        };
        let intent = Intent {
            rev,
            paused,
            live: base.live,
            rate_milli: base.rate_milli,
            position_ms: (pos_s.max(0.0) * 1000.0).round() as u64,
            ref_us: now_us,
            duration_ms: dur_ms,
            kind: base.kind,
            source: base.source,
        };
        let me = self.cfg.me;
        let scope = if mode == Mode::Coordinator {
            WriteScope::Room
        } else {
            WriteScope::Member
        };
        let prev = if scope == WriteScope::Room {
            self.room_rec.clone()
        } else {
            self.acting.get(&me).cloned().map(|i| (me, i))
        };
        let value = intent.encode();
        self.last_own = Some((scope, intent.clone()));
        if scope == WriteScope::Room {
            self.room_rec = Some((me, intent));
        } else {
            self.acting.insert(me, intent);
        }
        let token = self.token();
        self.writes.insert(
            token,
            WriteMeta {
                scope,
                prev,
                rev,
                body: body.unwrap_or_default(),
            },
        );
        out.push(Action::Write {
            token,
            scope,
            name: NAME_PLAYBACK,
            value,
        });
        // A new revision re-forces the next correction (the old `correct(new_revision)`).
        rev
    }

    /// Apply a body as the authority.
    fn apply_body(&mut self, now_us: u64, body: Body, out: &mut Vec<Action>) -> Option<u32> {
        let mode = self.auth?.mode;
        let cur = self.target(now_us).map(|(p, pa, _)| (p, pa));
        let base = self.effective().map(|(_, i)| i.clone());
        let (mut base, pos, paused) = if let Some((kind, src)) = &body.source {
            let live = *kind == SourceKind::Url && is_live_url(src);
            let b = Intent {
                rev: 0,
                paused: false,
                live,
                rate_milli: 1000,
                position_ms: 0,
                ref_us: now_us,
                duration_ms: 0,
                kind: *kind,
                source: src.clone(),
            };
            (
                b,
                body.seek_ms.unwrap_or(0) as f64 / 1000.0,
                body.paused.unwrap_or(false),
            )
        } else {
            let b = base?;
            if b.kind == SourceKind::None {
                return None;
            }
            let (p, pa) = cur.unwrap_or((b.position_ms as f64 / 1000.0, b.paused));
            (
                b,
                body.seek_ms.map_or(p, |s| s as f64 / 1000.0),
                body.paused.unwrap_or(pa),
            )
        };
        if base.live {
            base.rate_milli = 1000;
        }
        Some(self.write_intent(now_us, mode, base, pos, paused, Some(body), out))
    }

    fn on_write_result(&mut self, now_us: u64, token: u64, ok: bool, out: &mut Vec<Action>) {
        let Some(meta) = self.writes.remove(&token) else {
            return;
        };
        if ok {
            return;
        }
        // The daemon refused (we were not the coordinator after all). Undo, and send the
        // same change through the request path once the view settles.
        let me = self.cfg.me;
        match meta.scope {
            WriteScope::Room => {
                if self
                    .room_rec
                    .as_ref()
                    .is_some_and(|(w, i)| *w == me && i.rev == meta.rev)
                {
                    self.room_rec = meta.prev;
                }
            }
            WriteScope::Member => {
                if self.acting.get(&me).is_some_and(|i| i.rev == meta.rev) {
                    match meta.prev {
                        Some((_, i)) => {
                            self.acting.insert(me, i);
                        }
                        None => {
                            self.acting.remove(&me);
                        }
                    }
                }
            }
        }
        if !meta.body.is_empty() {
            self.enqueue(now_us, meta.body, Some(now_us + 500_000));
        }
        // Treat a refused write as a stale view of who coordinates.
        if self.coord.is_some_and(|c| c.holder == me) {
            self.blocked.insert(me, (now_us, 0));
        }
        let _ = out;
    }

    // ----- user commands --------------------------------------------------------

    fn on_command(&mut self, now_us: u64, c: UserCommand, out: &mut Vec<Action>) {
        let view = self.target(now_us).map(|(p, pa, _)| (p, pa));
        let dur = self.duration_s();
        let body = match c {
            UserCommand::Load { kind, source } => {
                if source.is_empty() || source.len() > MAX_SOURCE || kind == SourceKind::None {
                    return;
                }
                Body {
                    source: Some((kind, source)),
                    ..Default::default()
                }
            }
            UserCommand::Play | UserCommand::Pause | UserCommand::Toggle => {
                let cur = self
                    .pending
                    .as_ref()
                    .and_then(|p| p.body.paused)
                    .or_else(|| view.map(|v| v.1))
                    .or_else(|| self.effective().map(|(_, i)| i.paused));
                let Some(cur) = cur else { return };
                let paused = match c {
                    UserCommand::Play => false,
                    UserCommand::Pause => true,
                    _ => !cur,
                };
                Body {
                    paused: Some(paused),
                    ..Default::default()
                }
            }
            UserCommand::SeekTo(_) | UserCommand::SeekBy(_) => {
                let cur = self
                    .pending
                    .as_ref()
                    .and_then(|p| p.body.seek_ms)
                    .map(|ms| ms as f64 / 1000.0)
                    .or_else(|| view.map(|v| v.0))
                    .or_else(|| self.effective().map(|(_, i)| i.position_ms as f64 / 1000.0));
                let Some(cur) = cur else { return };
                let mut t = match c {
                    UserCommand::SeekTo(v) => v,
                    UserCommand::SeekBy(d) => cur + d.clamp(-3600.0, 3600.0),
                    _ => unreachable!(),
                };
                if !t.is_finite() {
                    return;
                }
                t = t.max(0.0);
                if dur > 0.0 {
                    t = t.min(dur);
                }
                Body {
                    seek_ms: Some((t * 1000.0).round() as u64),
                    ..Default::default()
                }
            }
        };
        if self.is_authority() {
            if self.apply_body(now_us, body.clone(), out).is_none() {
                // Nothing loaded to act on.
            }
        } else {
            self.enqueue(now_us, body, None);
        }
    }

    // ----- requests (follower side) -------------------------------------------------

    fn enqueue(&mut self, now_us: u64, body: Body, not_before_us: Option<u64>) {
        let req_id = self.next_req;
        self.next_req += 1;
        let deadline = now_us + self.cfg.request_ttl_ms * 1000;
        match &mut self.pending {
            Some(p) => {
                p.body.merge(body);
                p.req_id = req_id;
                p.deadline_us = deadline;
                p.attempt = 0;
                p.next_try_us = not_before_us.unwrap_or(now_us);
            }
            None => {
                self.pending = Some(Pending {
                    body,
                    req_id,
                    deadline_us: deadline,
                    next_try_us: not_before_us.unwrap_or(now_us),
                    attempt: 0,
                });
            }
        }
    }

    fn pump_request(&mut self, now_us: u64, out: &mut Vec<Action>) {
        let Some(p) = &self.pending else { return };
        if now_us >= p.deadline_us {
            self.pending = None;
            out.push(Action::Notice(Notice::ControlUnavailable));
            return;
        }
        if now_us < p.next_try_us {
            return;
        }
        let Some(auth) = self.auth else { return };
        if auth.key.holder == self.cfg.me {
            let p = self.pending.take().unwrap();
            self.apply_body(now_us, p.body, out);
            return;
        }
        let p = self.pending.as_mut().unwrap();
        let backoff = REQ_BACKOFF_MS[p.attempt.min(REQ_BACKOFF_MS.len() - 1)];
        p.attempt += 1;
        p.next_try_us = now_us + backoff * 1000;
        let req_id = p.req_id;
        let payload = Control::Request {
            req_id,
            key: auth.key,
            body: p.body.clone(),
        }
        .encode();
        let to = if auth.mode == Mode::Coordinator {
            Dest::Coordinator
        } else {
            Dest::Member(auth.key.holder)
        };
        let token = self.token();
        self.in_flight.insert(token, req_id);
        out.push(Action::Send { token, to, payload });
    }

    fn block(&mut self, k: Key, now_us: u64) {
        let count = self.blocked.get(&k).map_or(0, |b| b.1);
        let ms = self.cfg.block_ms << count.min(4);
        self.blocked.insert(k, (now_us + ms * 1000, count + 1));
    }

    fn alive(&mut self, k: &Key) {
        self.blocked.remove(k);
        self.retests.remove(k);
        self.fails.remove(k);
    }

    fn on_send_result(
        &mut self,
        now_us: u64,
        token: u64,
        outcome: SendOutcome,
        out: &mut Vec<Action>,
    ) {
        let Some(req_id) = self.in_flight.remove(&token) else {
            return;
        };
        if self.pending.as_ref().is_none_or(|p| p.req_id != req_id) {
            return;
        }
        match outcome {
            SendOutcome::Delivered => {}
            SendOutcome::NoChannel => {
                // The daemon is there but no player is registered on the channel.
                if let Some(a) = self.auth {
                    let n = self.fails.entry(a.key.holder).or_insert(0);
                    *n += 1;
                    if *n >= 2 {
                        self.fails.remove(&a.key.holder);
                        self.block(a.key.holder, now_us);
                        self.reevaluate(now_us, out);
                    }
                }
            }
            _ => {}
        }
        if outcome != SendOutcome::Delivered {
            if let Some(p) = self.pending.as_mut() {
                // Retry sooner than the reply timeout; the attempt counter keeps backing off.
                let b = REQ_BACKOFF_MS[p.attempt.saturating_sub(1).min(REQ_BACKOFF_MS.len() - 1)];
                p.next_try_us = now_us + b * 1000 / 2;
            }
        }
    }

    // ----- messages --------------------------------------------------------------

    fn send_reply(
        &mut self,
        to: Key,
        req_id: u32,
        status: ReplyStatus,
        rev: u32,
        out: &mut Vec<Action>,
    ) {
        let token = self.token();
        out.push(Action::Send {
            token,
            to: Dest::Member(to),
            payload: Control::Reply {
                req_id,
                status,
                rev,
            }
            .encode(),
        });
    }

    fn on_message(&mut self, now_us: u64, from: Key, payload: &[u8], out: &mut Vec<Action>) {
        let Ok(c) = Control::decode(payload) else {
            return;
        };
        match c {
            Control::Request { req_id, key, body } => {
                let ok = self
                    .auth
                    .is_some_and(|a| a.key == key && a.key.holder == self.cfg.me);
                if !ok {
                    self.send_reply(from, req_id, ReplyStatus::WrongAuthority, 0, out);
                    return;
                }
                if let Some((last, status, rev)) = self.last_from.get(&from).copied() {
                    if req_id == last {
                        self.send_reply(from, req_id, status, rev, out);
                        return;
                    }
                    if req_id < last {
                        // A stale reply carries the highest request id seen from this sender, so a
                        // restarted sender (ids back at 1) can jump past it.
                        let _ = rev;
                        self.send_reply(from, req_id, ReplyStatus::Stale, last, out);
                        return;
                    }
                }
                let valid = !body.is_empty()
                    && body.source.as_ref().is_none_or(|(k, s)| {
                        *k != SourceKind::None && !s.is_empty() && s.len() <= MAX_SOURCE
                    });
                let (status, rev) = if !valid {
                    (ReplyStatus::BadRequest, 0)
                } else {
                    match self.apply_body(now_us, body, out) {
                        Some(r) => (ReplyStatus::Applied, r),
                        None => (ReplyStatus::BadRequest, 0),
                    }
                };
                self.last_from.insert(from, (req_id, status, rev));
                self.send_reply(from, req_id, status, rev, out);
            }
            Control::Reply {
                req_id,
                status,
                rev,
            } => {
                self.alive(&from);
                if self.pending.as_ref().is_none_or(|p| p.req_id != req_id) {
                    return;
                }
                match status {
                    ReplyStatus::Applied | ReplyStatus::BadRequest => self.pending = None,
                    ReplyStatus::Stale => {
                        // Our ids are behind what the authority has seen from us (we
                        // restarted): this request is the newest we have, so re-send it
                        // with an id above the authority's.
                        if rev >= req_id {
                            self.next_req = self.next_req.max(rev.saturating_add(1));
                            let id = self.next_req;
                            self.next_req = self.next_req.saturating_add(1);
                            let p = self.pending.as_mut().unwrap();
                            p.req_id = id;
                            p.next_try_us = now_us;
                        } else {
                            self.pending = None;
                        }
                    }
                    ReplyStatus::WrongAuthority => {
                        self.pending.as_mut().unwrap().next_try_us = now_us + 500_000;
                    }
                }
            }
        }
    }

    fn on_datagram(&mut self, now_us: u64, from: Key, payload: &[u8], out: &mut Vec<Action>) {
        let Ok(p) = Probe::decode(payload) else {
            return;
        };
        match p {
            Probe::Ping { id, key, t1_us } => {
                if key.holder == self.cfg.me {
                    out.push(Action::Datagram {
                        to: from,
                        payload: Probe::Pong {
                            id,
                            key,
                            t1_us,
                            t2_us: now_us,
                            t3_us: now_us,
                        }
                        .encode(),
                    });
                }
            }
            Probe::Pong { id, .. } => {
                if id & 0x8000_0000 != 0 {
                    // Answer to a retest of a blocked member: it has a player again.
                    if self.retests.get(&from) == Some(&id) {
                        self.alive(&from);
                    }
                    return;
                }
                if Some(from) != self.est.key().map(|k| k.holder)
                    && !self.cfg.faults.probe_ignores_epoch
                {
                    return;
                }
                let first = !self.est.has_samples();
                if self.est.pong(now_us, &p) {
                    self.alive(&from);
                    // The first estimate after a rekey is the moment to jump to the room.
                    self.force_next |= first;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Time, target and the player
// ---------------------------------------------------------------------------

impl Cowatch {
    /// The authority's clock now, in microseconds, if known.
    fn host_now(&self, now_us: u64) -> Option<u64> {
        let (w, _) = self.effective()?;
        if w == self.cfg.me {
            return Some(now_us);
        }
        if self.est.key().map(|k| k.holder) != Some(w) {
            return None;
        }
        let off = self.est.offset_us(now_us)?;
        Some((now_us as i64).saturating_add(off).max(0) as u64)
    }

    fn duration_s(&self) -> f64 {
        if let Some(p) = &self.player {
            if self.loaded && p.duration_s > 0.0 {
                return p.duration_s;
            }
        }
        self.usable().map_or(0.0, |i| i.duration_ms as f64 / 1000.0)
    }

    /// Where the room is now: (position s, paused, rate). None without a usable record
    /// or without a clock estimate.
    fn target(&self, now_us: u64) -> Option<(f64, bool, f64)> {
        let rec = self.usable()?;
        if rec.kind == SourceKind::None {
            return None;
        }
        let host = self.host_now(now_us)?;
        let rate = rec.rate_milli as f64 / 1000.0;
        let mut pos = target_position(
            rec.position_ms as f64 / 1000.0,
            rec.ref_us as f64 / 1e6,
            host as f64 / 1e6,
            rec.paused,
            rate,
        );
        let dur = self.duration_s();
        if dur > 0.0 {
            pos = pos.min(dur);
        }
        Some((pos, rec.paused, rate))
    }

    fn on_player(&mut self, now_us: u64, r: PlayerReport, out: &mut Vec<Action>) {
        if r.token != self.load_token {
            return;
        }
        self.player = Some(r);
        if let Some(t0) = self.seek_started_us {
            if r.state == PlayerState::Seeking {
                self.seen_seeking = true;
            } else if self.seen_seeking || now_us.saturating_sub(t0) > 1_000_000 {
                // The seek has landed: learn how long it takes, and check the landing.
                let took = if self.seen_seeking {
                    now_us.saturating_sub(t0) as f64 / 1e6
                } else {
                    0.0
                };
                self.seek_lead_s = (0.5 * self.seek_lead_s + 0.5 * took).min(1.0);
                self.seek_started_us = None;
                self.seen_seeking = false;
                self.force_next = true;
            }
        }
        match r.state {
            PlayerState::Loading => {}
            PlayerState::Error => {
                if self.reload_at.is_none() && !self.failed {
                    self.loaded = true;
                    self.schedule_reload(now_us, r.position_s, out);
                }
            }
            PlayerState::Ended => {
                if !self.loaded {
                    self.loaded = true;
                    self.loaded_at_us = now_us;
                }
                if !self.finished && self.reload_at.is_none() && !self.failed {
                    self.on_ended(now_us, r, out);
                }
            }
            _ => {
                if !self.loaded {
                    self.loaded = true;
                    self.loaded_at_us = now_us;
                }
            }
        }
    }

    fn on_ended(&mut self, now_us: u64, r: PlayerReport, out: &mut Vec<Action>) {
        let live = self.usable().is_some_and(|i| i.live);
        let dur = if r.duration_s > 0.0 {
            r.duration_s
        } else {
            self.duration_s()
        };
        let clean = !self.cfg.faults.always_reload
            && !live
            && dur > 0.0
            && r.position_s >= dur - self.cfg.early_eof_slack_s;
        if clean {
            self.finished = true;
            out.push(Action::Notice(Notice::Finished));
        } else {
            self.schedule_reload(now_us, r.position_s, out);
        }
    }

    fn schedule_reload(&mut self, now_us: u64, last_pos: f64, out: &mut Vec<Action>) {
        let n = self.cfg.reload_backoff_ms.len() as u32;
        if self.reload_attempts >= n {
            self.failed = true;
            out.push(Action::Notice(Notice::Failed));
            return;
        }
        let delay = self.cfg.reload_backoff_ms[self.reload_attempts as usize];
        self.reload_attempts += 1;
        self.reload_at = Some((now_us + delay * 1000, last_pos));
        out.push(Action::Notice(Notice::Reloading {
            attempt: self.reload_attempts,
            of: n,
            in_ms: delay,
        }));
    }

    fn load_player(&mut self, now_us: u64, start_s: f64, out: &mut Vec<Action>) {
        let Some((kind, source)) = self.player_src.clone() else {
            return;
        };
        self.load_token += 1;
        self.loaded = false;
        self.player = None;
        self.awaiting_seek = true;
        self.was_buffering = false;
        self.finished = false;
        self.cmd_speed = None;
        self.cmd_paused = None;
        self.seek_hold_until_us = 0;
        self.drift_s = None;
        self.reload_at = None;
        self.last_correct_us = 0;
        self.seek_started_us = None;
        self.seen_seeking = false;
        let _ = now_us;
        out.push(Action::Player(PlayerCmd::Load {
            token: self.load_token,
            kind,
            source,
            start_s,
        }));
    }

    // ----- periodic work ----------------------------------------------------------

    fn run(&mut self, now_us: u64, out: &mut Vec<Action>) {
        // Blocks expire silently; re-derive the authority every pass.
        self.reevaluate(now_us, out);
        self.adopt(now_us, out);
        self.sync_clock(now_us);
        // Retest blocked members now and then; a pong unblocks them.
        let due: Vec<Key> = self
            .blocked
            .iter()
            .filter(|(k, (until, _))| {
                now_us >= *until
                    && self.caps.contains_key(*k)
                    && matches!(self.members.get(*k), None | Some(MemberState::Online))
            })
            .map(|(k, _)| *k)
            .collect();
        for k in due {
            let id = self.next_retest_id;
            self.next_retest_id = 0x8000_0000 | self.next_retest_id.wrapping_add(1);
            self.retests.insert(k, id);
            let (g, t) = self.coord.map_or((0, 0), |c| (c.generation, c.term));
            out.push(Action::Datagram {
                to: k,
                payload: Probe::Ping {
                    id,
                    key: AuthKey {
                        generation: g,
                        term: t,
                        holder: k,
                    },
                    t1_us: now_us,
                }
                .encode(),
            });
            if let Some(b) = self.blocked.get_mut(&k) {
                b.0 = now_us + self.cfg.block_ms * 1000;
            }
        }
        // clock probes
        if let Some(k) = self.est.key() {
            if now_us >= self.next_probe_us {
                if let Some(p) = self.est.ping(now_us) {
                    out.push(Action::Datagram {
                        to: k.holder,
                        payload: p.encode(),
                    });
                }
                self.next_probe_us = now_us
                    + if self.burst_left > 0 {
                        150_000
                    } else {
                        self.cfg.probe_interval_ms * 1000
                    };
                self.burst_left = self.burst_left.saturating_sub(1);
            }
            // A writer that never answers probes is as good as absent.
            let quiet = self
                .est
                .last_sample_us()
                .map_or(now_us.saturating_sub(self.probe_since_us), |t| {
                    now_us.saturating_sub(t)
                });
            if now_us.saturating_sub(self.probe_since_us) > 8_000_000 && quiet > 6_000_000 {
                self.probe_since_us = now_us;
                self.block(k.holder, now_us);
                self.reevaluate(now_us, out);
            }
        }
        self.pump_request(now_us, out);
        if let Some((at, pos)) = self.reload_at {
            if now_us >= at {
                let start = self.target(now_us).map_or(pos, |t| t.0);
                self.load_player(now_us, start, out);
            }
        }
        self.drive(now_us, out);
    }

    fn drive(&mut self, now_us: u64, out: &mut Vec<Action>) {
        if self.auth.is_none() {
            return;
        }
        let Some(rec) = self.usable().cloned() else {
            return;
        };
        if rec.kind == SourceKind::None {
            if self.player_src.take().is_some() {
                self.loaded = false;
                self.player = None;
                out.push(Action::Player(PlayerCmd::Stop));
            }
            return;
        }
        let src = (rec.kind, rec.source.clone());
        if self.player_src.as_ref() != Some(&src) {
            self.player_src = Some(src);
            self.reload_attempts = 0;
            self.failed = false;
            self.seen_rev = None;
            let start = self.target(now_us).map_or(0.0, |t| t.0);
            self.load_player(now_us, start, out);
            return;
        }
        if self.seen_rev != Some(rec.rev) {
            self.seen_rev = Some(rec.rev);
            self.force_next = true;
            if self.failed {
                // A new revision is the user asking again: start over.
                self.failed = false;
                self.reload_attempts = 0;
                self.reload_at = Some((now_us, 0.0));
            }
        }
        if !self.loaded || self.reload_at.is_some() || self.failed {
            return;
        }
        let Some(p) = self.player else { return };
        if self.reload_attempts > 0 && now_us.saturating_sub(self.loaded_at_us) > 10_000_000 {
            self.reload_attempts = 0;
        }
        if rec.live {
            self.awaiting_seek = false;
            self.drift_s = Some(0.0);
            self.set_speed(1.0, out);
            self.set_paused(false, out);
            return;
        }
        let Some((tpos, paused, rate)) = self.target(now_us) else {
            return;
        };
        let dur = self.duration_s();
        if self.finished {
            if dur > 0.0 && tpos < dur - self.cfg.early_eof_slack_s {
                self.load_player(now_us, tpos, out);
            }
            return;
        }
        if p.state == PlayerState::Ended {
            return;
        }
        let due =
            now_us.saturating_sub(self.last_correct_us) >= self.cfg.correct_interval_ms * 1000;
        if !due && !self.force_next {
            return;
        }
        match p.state {
            PlayerState::Buffering => {
                self.was_buffering = true;
                return;
            }
            PlayerState::Seeking | PlayerState::Loading => return,
            _ => {}
        }
        if now_us < self.seek_hold_until_us {
            return;
        }
        self.last_correct_us = now_us;
        let mut force = self.force_next;
        self.force_next = false;
        if self.was_buffering {
            force = true;
            self.was_buffering = false;
        }
        // Not in the old code: a drift the speed nudge cannot remove in reasonable time (it
        // closes 20 ms per second at most) becomes a seek after it has persisted.
        let current = (tpos - p.position_s).abs();
        if current > self.cfg.persist_drift_s {
            let since = *self.drift_over_since_us.get_or_insert(now_us);
            if now_us.saturating_sub(since) >= self.cfg.persist_after_ms * 1000 {
                force = true;
            }
        } else {
            self.drift_over_since_us = None;
        }
        let d = correct(
            &self.cfg.params,
            &Inputs {
                target_s: tpos,
                position_s: p.position_s,
                rate,
                desired_paused: paused,
                force,
                awaiting_seek: self.awaiting_seek,
            },
        );
        self.drift_s = Some(d.drift_s);
        if self.cfg.faults.no_correction {
            self.set_paused(d.paused, out);
            return;
        }
        if let Some(to) = d.seek_to_s {
            // The room keeps moving while the seek is in flight: aim ahead by what the last
            // seeks took (not in the old code; the pure decision above is unchanged).
            let lead = if paused { 0.0 } else { self.seek_lead_s * rate };
            let to = if dur > 0.0 {
                (to + lead).min(dur)
            } else {
                to + lead
            };
            out.push(Action::Player(PlayerCmd::Seek(to)));
            self.seek_started_us = Some(now_us);
            self.seen_seeking = false;
            self.drift_over_since_us = None;
            self.awaiting_seek = false;
            self.seek_hold_until_us = now_us + 400_000;
        }
        self.set_speed(d.speed, out);
        self.set_paused(d.paused, out);
    }

    fn set_speed(&mut self, s: f64, out: &mut Vec<Action>) {
        if self.cmd_speed.is_none_or(|c| (c - s).abs() > 1e-9) {
            self.cmd_speed = Some(s);
            out.push(Action::Player(PlayerCmd::SetSpeed(s)));
        }
    }

    fn set_paused(&mut self, p: bool, out: &mut Vec<Action>) {
        if self.cmd_paused != Some(p) {
            self.cmd_paused = Some(p);
            out.push(Action::Player(PlayerCmd::SetPaused(p)));
        }
    }
}

impl Cowatch {
    #[doc(hidden)]
    pub fn debug_state(&self) -> String {
        format!(
            "src={:?} token={} loaded={} player={:?} reload_at={:?} failed={} finished={} usable={}",
            self.player_src.as_ref().map(|s| s.1.len()),
            self.load_token,
            self.loaded,
            self.player,
            self.reload_at,
            self.failed,
            self.finished,
            self.usable().is_some()
        )
    }
}
