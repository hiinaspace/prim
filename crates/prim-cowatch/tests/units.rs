//! Machine-level tests with hand-fed events: requests, probes, early EOF, and the
//! faults shown red.
use prim_cowatch::wire::*;
use prim_cowatch::*;

fn k(b: u8) -> Key {
    [b; 32]
}
const A: u8 = 0xa;
const B: u8 = 0xb;
const C: u8 = 0xc;
const S: u64 = 1_000_000;

fn coord(term: u32, who: u8) -> Event {
    Event::Coordinator(Some(CoordView {
        generation: 1,
        term,
        holder: k(who),
        state: CoordState::Live,
    }))
}
fn cap(who: u8, since: u64) -> Event {
    Event::Cap {
        writer: k(who),
        value: Cap { since_ms: since }.encode(),
    }
}
fn machine(me: u8, since: u64, faults: Faults) -> Cowatch {
    let mut cfg = Config::new(k(me), since);
    cfg.faults = faults;
    let mut m = Cowatch::new(cfg, 0).0;
    m.handle(0, Event::Synced);
    m
}
fn intent(rev: u32, pos_s: u64, ref_us: u64) -> Intent {
    Intent {
        rev,
        paused: false,
        live: false,
        rate_milli: 1000,
        position_ms: pos_s * 1000,
        ref_us,
        duration_ms: 100_000,
        kind: SourceKind::Url,
        source: b"https://x.test/v".to_vec(),
    }
}
fn room_rec(who: u8, i: &Intent) -> Event {
    Event::Playback {
        room: true,
        writer: k(who),
        value: i.encode(),
    }
}
fn pings(acts: &[Action]) -> Vec<Probe> {
    acts.iter()
        .filter_map(|a| match a {
            Action::Datagram { payload, .. } => Probe::decode(payload).ok(),
            _ => None,
        })
        .collect()
}
fn sends(acts: &[Action]) -> Vec<(u64, Dest, Control)> {
    acts.iter()
        .filter_map(|a| match a {
            Action::Send { token, to, payload } => {
                Some((*token, *to, Control::decode(payload).unwrap()))
            }
            _ => None,
        })
        .collect()
}
fn writes(acts: &[Action]) -> Vec<(WriteScope, &'static str, Vec<u8>)> {
    acts.iter()
        .filter_map(|a| match a {
            Action::Write {
                scope, name, value, ..
            } => Some((*scope, *name, value.clone())),
            _ => None,
        })
        .collect()
}
fn has_notice(acts: &[Action], n: Notice) -> bool {
    acts.contains(&Action::Notice(n))
}

// ---------------------------------------------------------------------------------
// A takeover in the middle of a clock probe
// ---------------------------------------------------------------------------------

/// B follows A. A ping to A is in flight; A is replaced by C (a new coordinator term whose
/// clock is a thousand seconds away); only then does A's pong, generated before the
/// takeover, arrive. Returns the clock offset B ends up believing for C.
fn takeover_mid_probe(faults: Faults) -> (Option<i64>, Option<i64>) {
    let mut b = machine(B, 20, faults);
    b.handle(0, cap(A, 1));
    b.handle(0, cap(C, 2));
    b.handle(0, coord(0, A));
    b.handle(0, room_rec(A, &intent(1, 10, 5 * S)));
    // B probes A.
    let acts = b.handle(S, Event::Tick);
    let Probe::Ping { id, key, t1_us } = pings(&acts)[0] else {
        panic!()
    };
    assert_eq!(key.holder, k(A));
    // Takeover: C becomes coordinator and writes a newer record in its own clock.
    b.handle(S + 10_000, coord(1, C));
    b.handle(
        S + 10_000,
        Event::Member {
            key: k(A),
            state: MemberState::Unresponsive,
        },
    );
    b.handle(S + 20_000, room_rec(C, &intent(2, 11, 2_000 * S)));
    // A's old pong: A's clock reads ~1000 s ahead of B's, C's reads ~0 ahead.
    let pong = Probe::Pong {
        id,
        key,
        t1_us,
        t2_us: t1_us + 1000 * S + 20_000,
        t3_us: t1_us + 1000 * S + 20_001,
    };
    b.handle(
        S + 60_000,
        Event::Datagram {
            from: k(A),
            payload: pong.encode(),
        },
    );
    let stale = b.view(S + 60_000).clock_offset_us;
    // Then the real thing: C answers a ping addressed to C.
    let acts = b.handle(S + 200_000, Event::Tick);
    let ping = pings(&acts)
        .into_iter()
        .find(|p| matches!(p, Probe::Ping { key, .. } if key.holder == k(C)))
        .expect("a ping to C");
    let Probe::Ping { id, key, t1_us } = ping else {
        panic!()
    };
    let pong = Probe::Pong {
        id,
        key,
        t1_us,
        t2_us: t1_us + 20_000,
        t3_us: t1_us + 20_001,
    };
    b.handle(
        S + 240_000,
        Event::Datagram {
            from: k(C),
            payload: pong.encode(),
        },
    );
    (stale, b.view(S + 240_000).clock_offset_us)
}

#[test]
fn a_pong_from_the_old_holder_never_becomes_an_estimate_for_the_new_one() {
    let (stale, good) = takeover_mid_probe(Faults::default());
    assert_eq!(
        stale, None,
        "an estimate appeared from the old epoch's pong"
    );
    let off = good.expect("no estimate from C");
    assert!(
        off.abs() < 50_000,
        "offset to C should be tens of ms, got {off}"
    );
}

#[test]
fn red_without_the_epoch_key_the_old_pong_poisons_the_estimate() {
    let (stale, _) = takeover_mid_probe(Faults {
        probe_ignores_epoch: true,
        ..Default::default()
    });
    let off = stale.expect("fault: the stale pong was accepted");
    assert!(
        off > 900 * S as i64,
        "the oracle must see a ~1000 s error, got {off}"
    );
}

// ---------------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------------

/// A is the authority (acting mode, no coordinator known) with a record in force.
fn authority_with_record() -> Cowatch {
    let mut a = machine(A, 1, Faults::default());
    a.handle(0, cap(B, 5));
    let acts = a.handle(
        S,
        Event::Command(UserCommand::Load {
            kind: SourceKind::Url,
            source: b"https://x.test/v".to_vec(),
        }),
    );
    assert_eq!(writes(&acts).len(), 1);
    a
}

fn req(a: &Cowatch, id: u32, body: Body) -> Event {
    let key = a.view(0).authority.unwrap().key;
    Event::Message {
        from: k(B),
        payload: Control::Request {
            req_id: id,
            key,
            body,
        }
        .encode(),
    }
}
fn paused(p: bool) -> Body {
    Body {
        paused: Some(p),
        ..Default::default()
    }
}
fn reply_of(acts: &[Action]) -> (u32, ReplyStatus, u32) {
    match &sends(acts)[0].2 {
        Control::Reply {
            req_id,
            status,
            rev,
        } => (*req_id, *status, *rev),
        o => panic!("{o:?}"),
    }
}

#[test]
fn a_duplicate_request_is_answered_again_but_applied_once() {
    let mut a = authority_with_record();
    let acts = a.handle(2 * S, req(&a, 7, paused(true)));
    assert_eq!(writes(&acts).len(), 1);
    let (_, st, rev) = reply_of(&acts);
    assert_eq!(st, ReplyStatus::Applied);
    let acts = a.handle(3 * S, req(&a, 7, paused(true)));
    assert!(writes(&acts).is_empty(), "applied twice");
    assert_eq!(reply_of(&acts), (7, ReplyStatus::Applied, rev));
}

#[test]
fn a_reordered_older_request_is_stale() {
    let mut a = authority_with_record();
    a.handle(2 * S, req(&a, 9, paused(true)));
    let acts = a.handle(3 * S, req(&a, 8, paused(false)));
    assert!(writes(&acts).is_empty());
    assert_eq!(reply_of(&acts).1, ReplyStatus::Stale);
}

#[test]
fn requests_for_another_epoch_or_holder_or_nothing_are_refused() {
    let mut a = authority_with_record();
    let mut key = a.view(0).authority.unwrap().key;
    key.term += 1;
    let acts = a.handle(
        S,
        Event::Message {
            from: k(B),
            payload: Control::Request {
                req_id: 1,
                key,
                body: paused(true),
            }
            .encode(),
        },
    );
    assert_eq!(reply_of(&acts).1, ReplyStatus::WrongAuthority);
    assert!(writes(&acts).is_empty());
    let acts = a.handle(S, req(&a, 2, Body::default()));
    assert_eq!(reply_of(&acts).1, ReplyStatus::BadRequest);
    // Garbage on the channel is dropped without a reply.
    let acts = a.handle(
        S,
        Event::Message {
            from: k(B),
            payload: vec![0xff, 1, 2],
        },
    );
    assert!(sends(&acts).is_empty() && writes(&acts).is_empty());
}

#[test]
fn a_follower_retries_on_no_coordinator_no_channel_and_lost_then_stops_on_the_reply() {
    // B follows A (A is the coordinator and has a player).
    let mut b = machine(B, 20, Faults::default());
    b.handle(0, cap(A, 1));
    b.handle(0, coord(0, A));
    b.handle(0, room_rec(A, &intent(1, 10, 5 * S)));
    let acts = b.handle(S, Event::Command(UserCommand::Pause));
    let s1 = sends(&acts);
    assert_eq!(s1.len(), 1);
    assert_eq!(s1[0].1, Dest::Coordinator);
    let Control::Request { req_id, .. } = s1[0].2.clone() else {
        panic!()
    };
    // The daemon says there is no coordinator; the request is not dropped.
    b.handle(
        S,
        Event::SendResult {
            token: s1[0].0,
            outcome: SendOutcome::NoCoordinator,
        },
    );
    let acts = b.handle(S + 200_000, Event::Tick);
    let s2 = sends(&acts);
    assert_eq!(s2.len(), 1, "no retry after NoCoordinator");
    b.handle(
        S + 200_000,
        Event::SendResult {
            token: s2[0].0,
            outcome: SendOutcome::Lost,
        },
    );
    let acts = b.handle(S + 600_000, Event::Tick);
    let s3 = sends(&acts);
    assert_eq!(s3.len(), 1, "no retry after Lost");
    // Delivered, but the application never answers: it retries after the reply timeout.
    b.handle(
        S + 600_000,
        Event::SendResult {
            token: s3[0].0,
            outcome: SendOutcome::Delivered,
        },
    );
    let acts = b.handle(S + 600_000 + 3_000_000, Event::Tick);
    let s4 = sends(&acts);
    assert_eq!(s4.len(), 1);
    // The reply ends it.
    let Control::Request { req_id: same, .. } = s4[0].2.clone() else {
        panic!()
    };
    assert_eq!(
        same, req_id,
        "a retry keeps its id so the authority can dedupe"
    );
    b.handle(
        S + 4_000_000,
        Event::Message {
            from: k(A),
            payload: Control::Reply {
                req_id,
                status: ReplyStatus::Applied,
                rev: 2,
            }
            .encode(),
        },
    );
    assert!(!b.view(S + 4_000_000).request_pending);
    assert!(sends(&b.handle(S + 20_000_000, Event::Tick)).is_empty());
}

#[test]
fn no_channel_means_the_coordinators_player_is_down_so_a_member_with_a_player_acts() {
    // A is the coordinator with a capability record but no listener. B asks twice, gets
    // NoChannel twice, and (being the only other player) becomes the acting authority and
    // applies its own request.
    let mut b = machine(B, 20, Faults::default());
    b.handle(0, cap(A, 1));
    b.handle(0, coord(0, A));
    b.handle(0, room_rec(A, &intent(1, 10, 5 * S)));
    let acts = b.handle(S, Event::Command(UserCommand::Pause));
    let s1 = sends(&acts);
    b.handle(
        S,
        Event::SendResult {
            token: s1[0].0,
            outcome: SendOutcome::NoChannel,
        },
    );
    let acts = b.handle(S + 300_000, Event::Tick);
    let s2 = sends(&acts);
    assert_eq!(s2.len(), 1);
    let acts = b.handle(
        S + 300_000,
        Event::SendResult {
            token: s2[0].0,
            outcome: SendOutcome::NoChannel,
        },
    );
    let _ = acts;
    // Without a clock estimate for A it cannot project A's timeline, so after the
    // adoption wait it continues paused at the last known position.
    let mut acts = b.handle(S + 400_000, Event::Tick);
    let mut t = S + 400_000;
    while t < S + 10_000_000 && writes(&acts).is_empty() {
        t += 250_000;
        acts = b.handle(t, Event::Tick);
    }
    let v = b.view(t);
    assert!(v.is_authority, "B should act: {v:?}");
    assert_eq!(v.authority.unwrap().mode, Mode::Acting);
    // It wrote a member-scope record that is paused, and the request is no longer pending.
    let w: Vec<_> = writes(&acts);
    assert!(
        w.iter().any(|x| x.0 == WriteScope::Member
            && x.1 == NAME_PLAYBACK
            && Intent::decode(&x.2).unwrap().paused),
        "{acts:?}"
    );
    assert!(!v.request_pending);
}

#[test]
fn queued_requests_merge_into_one_with_a_newer_id() {
    let mut b = machine(B, 20, Faults::default());
    b.handle(0, cap(A, 1));
    b.handle(0, coord(0, A));
    b.handle(0, room_rec(A, &intent(1, 10, 5 * S)));
    // No clock yet, so SeekTo needs the record's own position: still allowed.
    let a1 = b.handle(S, Event::Command(UserCommand::Pause));
    let a2 = b.handle(S + 1, Event::Command(UserCommand::SeekTo(50.0)));
    let first = match &sends(&a1)[0].2 {
        Control::Request { req_id, .. } => *req_id,
        _ => panic!(),
    };
    // The second command re-sends immediately with both fields and a newer id.
    let s = sends(&a2);
    assert_eq!(s.len(), 1);
    match &s[0].2 {
        Control::Request { req_id, body, .. } => {
            assert!(*req_id > first);
            assert_eq!(body.paused, Some(true));
            assert_eq!(body.seek_ms, Some(50_000));
        }
        _ => panic!(),
    }
}

#[test]
fn a_request_with_nobody_to_ask_expires_with_a_notice() {
    // Alone, with no capability anywhere else: B is its own authority, so ask for
    // something it cannot do (nothing loaded): ignored, no pending request.
    let mut b = machine(B, 20, Faults::default());
    let acts = b.handle(S, Event::Command(UserCommand::Pause));
    assert!(acts.is_empty());
    // With an authority that never answers: the request lapses after its ttl.
    let mut b = machine(B, 20, Faults::default());
    b.handle(0, cap(A, 1));
    b.handle(0, coord(0, A));
    b.handle(0, room_rec(A, &intent(1, 10, 5 * S)));
    b.handle(S, Event::Command(UserCommand::Pause));
    let mut seen = false;
    let mut t = S;
    while t < 40 * S {
        t += 250_000;
        let acts = b.handle(t, Event::Tick);
        for s in sends(&acts) {
            b.handle(
                t,
                Event::SendResult {
                    token: s.0,
                    outcome: SendOutcome::Delivered,
                },
            );
        }
        // The probe silence rule may hand authority to B itself; either way the request ends.
        seen |= has_notice(&acts, Notice::ControlUnavailable);
        if !b.view(t).request_pending {
            break;
        }
    }
    assert!(
        !b.view(t).request_pending,
        "request never ended (seen notice: {seen})"
    );
}

// ---------------------------------------------------------------------------------
// Early end of file
// ---------------------------------------------------------------------------------

/// A, alone, plays a 100 s file; returns the machine and the load token.
fn playing_alone(faults: Faults) -> (Cowatch, u32) {
    let mut cfg = Config::new(k(A), 1);
    cfg.faults = faults;
    let (mut a, _) = Cowatch::new(cfg, 0);
    a.handle(0, Event::Synced);
    let acts = a.handle(
        S,
        Event::Command(UserCommand::Load {
            kind: SourceKind::Url,
            source: b"https://x.test/v".to_vec(),
        }),
    );
    let token = acts.iter().find_map(|x| {
        if let Action::Player(PlayerCmd::Load { token, .. }) = x {
            Some(*token)
        } else {
            None
        }
    });
    let token = token.unwrap_or_else(|| {
        let acts = a.handle(S + 50_000, Event::Tick);
        acts.iter()
            .find_map(|x| {
                if let Action::Player(PlayerCmd::Load { token, .. }) = x {
                    Some(*token)
                } else {
                    None
                }
            })
            .expect("no load")
    });
    let r = PlayerReport {
        token,
        state: PlayerState::Ready,
        position_s: 0.0,
        duration_s: 100.0,
        paused: true,
    };
    a.handle(S + 400_000, Event::Player(r));
    (a, token)
}

fn end_at(a: &mut Cowatch, token: u32, pos: f64, dur: f64, t: u64) -> Vec<Action> {
    a.handle(
        t,
        Event::Player(PlayerReport {
            token,
            state: PlayerState::Ended,
            position_s: pos,
            duration_s: dur,
            paused: false,
        }),
    )
}

#[test]
fn an_end_more_than_three_seconds_early_reloads_with_backoff_and_never_finishes() {
    let (mut a, token) = playing_alone(Faults::default());
    let acts = end_at(&mut a, token, 50.0, 100.0, 10 * S);
    assert!(!has_notice(&acts, Notice::Finished));
    assert!(
        has_notice(
            &acts,
            Notice::Reloading {
                attempt: 1,
                of: 6,
                in_ms: 500
            }
        ),
        "{acts:?}"
    );
    // Not before the backoff, then exactly one Load.
    let acts = a.handle(10 * S + 400_000, Event::Tick);
    assert!(!acts
        .iter()
        .any(|x| matches!(x, Action::Player(PlayerCmd::Load { .. }))));
    let acts = a.handle(10 * S + 500_000, Event::Tick);
    let loads: Vec<_> = acts
        .iter()
        .filter(|x| matches!(x, Action::Player(PlayerCmd::Load { .. })))
        .collect();
    assert_eq!(loads.len(), 1);
    // The reload fails the same way: backoff doubles, up to the cap, then Failed.
    let mut tok = token + 1;
    let mut t = 11 * S;
    let mut delays = vec![500];
    for _ in 0..8 {
        let acts = a.handle(
            t,
            Event::Player(PlayerReport {
                token: tok,
                state: PlayerState::Error,
                position_s: 0.0,
                duration_s: 0.0,
                paused: true,
            }),
        );
        if has_notice(&acts, Notice::Failed) {
            break;
        }
        let d = acts
            .iter()
            .find_map(|x| {
                if let Action::Notice(Notice::Reloading { in_ms, .. }) = x {
                    Some(*in_ms)
                } else {
                    None
                }
            })
            .unwrap();
        delays.push(d);
        t += d * 1000;
        let acts = a.handle(t, Event::Tick);
        tok = acts
            .iter()
            .find_map(|x| {
                if let Action::Player(PlayerCmd::Load { token, .. }) = x {
                    Some(*token)
                } else {
                    None
                }
            })
            .expect("reload");
        t += 1000;
    }
    assert_eq!(delays, vec![500, 1000, 2000, 4000, 8000, 16000]);
    assert!(a.view(t).failed, "{delays:?}");
}

#[test]
fn the_boundary_is_three_seconds_and_a_clean_end_finishes_promptly() {
    let (mut a, token) = playing_alone(Faults::default());
    let acts = end_at(&mut a, token, 97.0, 100.0, 10 * S);
    assert!(
        has_notice(&acts, Notice::Finished),
        "exactly 3.0 s early is a clean end: {acts:?}"
    );
    let (mut a, token) = playing_alone(Faults::default());
    let acts = end_at(&mut a, token, 96.99, 100.0, 10 * S);
    assert!(
        !has_notice(&acts, Notice::Finished)
            && acts
                .iter()
                .any(|x| matches!(x, Action::Notice(Notice::Reloading { .. })))
    );
    // An end with no known duration cannot be called clean.
    let (mut a, token) = playing_alone(Faults::default());
    let acts = end_at(&mut a, token, 99.9, 0.0, 10 * S);
    // The report carries no duration and the record has none, so it is not called clean.
    assert!(!has_notice(&acts, Notice::Finished), "{acts:?}");
}

#[test]
fn red_always_reload_turns_a_clean_end_into_a_reload() {
    let (mut a, token) = playing_alone(Faults {
        always_reload: true,
        ..Default::default()
    });
    let acts = end_at(&mut a, token, 100.0, 100.0, 10 * S);
    assert!(!has_notice(&acts, Notice::Finished));
}

#[test]
fn stale_reports_from_a_previous_load_are_ignored() {
    let (mut a, token) = playing_alone(Faults::default());
    end_at(&mut a, token, 50.0, 100.0, 10 * S);
    a.handle(10 * S + 500_000, Event::Tick); // reload: new token
    let acts = end_at(&mut a, token, 50.0, 100.0, 11 * S); // the old player instance again
    assert!(acts.iter().all(|x| !matches!(x, Action::Notice(_))));
}

// ---------------------------------------------------------------------------------
// Record choice
// ---------------------------------------------------------------------------------

#[test]
fn a_new_authority_continues_the_timeline_and_the_higher_revision_wins() {
    // B follows A, learns the clock, then A vanishes and B (next in line) takes over.
    let mut b = machine(B, 20, Faults::default());
    b.handle(0, cap(A, 1));
    b.handle(0, coord(0, A));
    b.handle(0, room_rec(A, &intent(4, 10, 5 * S)));
    let acts = b.handle(S, Event::Tick);
    let Probe::Ping { id, key, t1_us } = pings(&acts)[0] else {
        panic!()
    };
    // A's clock is exactly B's + 2 s; 20 ms each way.
    let t4 = t1_us + 40_000;
    let pong = Probe::Pong {
        id,
        key,
        t1_us,
        t2_us: t1_us + 20_000 + 2 * S,
        t3_us: t1_us + 20_000 + 2 * S,
    };
    b.handle(
        t4,
        Event::Datagram {
            from: k(A),
            payload: pong.encode(),
        },
    );
    // At B-time 4 s, A-time is 6 s: the room is at 10 + (6-5) = 11 s. A goes away.
    let acts = b.handle(
        4 * S,
        Event::Member {
            key: k(A),
            state: MemberState::Unresponsive,
        },
    );
    let w = writes(&acts);
    assert_eq!(w.len(), 1, "{acts:?}");
    let rec = Intent::decode(&w[0].2).unwrap();
    assert_eq!(rec.rev, 5, "revision continues");
    assert!(
        (rec.position_ms as i64 - 11_000).abs() < 30,
        "continues at the room position: {}",
        rec.position_ms
    );
    assert_eq!(rec.ref_us, 4 * S);
}

// ---------------------------------------------------------------------------------
// Found by the independent review of 728753d
// ---------------------------------------------------------------------------------

/// B follows A (both have players; A is the coordinator and the record is A's).
fn follower_with_record() -> Cowatch {
    let mut b = machine(B, 20, Faults::default());
    b.handle(0, cap(A, 1));
    b.handle(0, coord(0, A));
    b.handle(0, room_rec(A, &intent(1, 10, 5 * S)));
    b
}

#[test]
fn a_restarted_requester_whose_ids_are_behind_is_not_ignored() {
    let mut a = authority_with_record();
    // The old process got to id 9.
    a.handle(2 * S, req(&a, 9, paused(true)));
    // The new process starts at 1: the authority says stale and names its last id ...
    let acts = a.handle(3 * S, req(&a, 1, paused(false)));
    assert_eq!(reply_of(&acts), (1, ReplyStatus::Stale, 9));
    // ... and a requester that gets that answer re-sends with a higher id, once.
    let mut b = follower_with_record();
    let acts = b.handle(S, Event::Command(UserCommand::Pause));
    let Control::Request { req_id, .. } = sends(&acts)[0].2.clone() else {
        panic!()
    };
    let acts = b.handle(
        S + 1,
        Event::Message {
            from: k(A),
            payload: Control::Reply {
                req_id,
                status: ReplyStatus::Stale,
                rev: 9,
            }
            .encode(),
        },
    );
    assert!(b.view(S + 1).request_pending, "dropped on Stale");
    let s = sends(&acts);
    assert_eq!(s.len(), 1);
    let Control::Request {
        req_id: again,
        body,
        ..
    } = s[0].2.clone()
    else {
        panic!()
    };
    assert!(again > 9, "{again}");
    assert_eq!(body.paused, Some(true));
}

#[test]
fn rapid_relative_controls_stack_on_the_pending_request() {
    let mut b = follower_with_record();
    // No clock yet: the base is the record's own 10 s. The last request on the wire
    // carries 40 s, not 20 s.
    let _ = &mut b;
    let mut b = follower_with_record();
    let mut last = None;
    for n in 0..3 {
        let acts = b.handle(S + n * 50_000, Event::Command(UserCommand::SeekBy(10.0)));
        if let Some(x) = sends(&acts).pop() {
            last = Some(x.2);
        }
    }
    let Some(Control::Request { body, .. }) = last else {
        panic!()
    };
    assert_eq!(body.seek_ms, Some(40_000));
    // Two toggles inside one round trip cancel out.
    let mut b = follower_with_record();
    b.handle(S, Event::Command(UserCommand::Toggle));
    let acts = b.handle(S + 10_000, Event::Command(UserCommand::Toggle));
    let Control::Request { body, .. } = sends(&acts)[0].2.clone() else {
        panic!()
    };
    assert_eq!(body.paused, Some(false));
}

#[test]
fn hostile_values_neither_panic_nor_freeze_the_room() {
    let mut b = follower_with_record();
    // A revision at the top of the range is ignored.
    let mut evil = intent(u32::MAX, 10, 5 * S);
    evil.rev = u32::MAX;
    b.handle(
        S,
        Event::Playback {
            room: false,
            writer: k(C),
            value: evil.encode(),
        },
    );
    assert_eq!(b.view(S).rev, Some(1));
    // A pong from the writer with absurd timestamps is dropped.
    let acts = b.handle(S + 2_000_000, Event::Tick);
    let ps = pings(&acts);
    assert!(!ps.is_empty(), "{acts:?}");
    let Probe::Ping { id, key, t1_us } = ps[0] else {
        panic!()
    };
    let pong = Probe::Pong {
        id,
        key,
        t1_us,
        t2_us: 1 << 63,
        t3_us: 1 << 63,
    };
    b.handle(
        S + 1000,
        Event::Datagram {
            from: k(A),
            payload: pong.encode(),
        },
    );
    assert_eq!(b.view(S + 1000).clock_offset_us, None);
    // A host clock that steps backwards does not panic.
    b.handle(10, Event::Tick);
}

#[test]
fn the_record_vanishing_before_the_member_event_still_leaves_a_timeline() {
    // B and C follow A, who is acting (coordinator has no player). A's record is removed
    // first, then A's member event arrives; B takes over and Pause still works.
    let mut b = machine(B, 20, Faults::default());
    b.handle(0, cap(A, 1));
    b.handle(0, cap(C, 30));
    let ra = intent(1, 10, 5 * S);
    b.handle(
        0,
        Event::Playback {
            room: false,
            writer: k(A),
            value: ra.encode(),
        },
    );
    b.handle(
        S,
        Event::PlaybackRemoved {
            room: false,
            writer: Some(k(A)),
        },
    );
    let acts = b.handle(
        S,
        Event::Member {
            key: k(A),
            state: MemberState::Offline,
        },
    );
    let _ = acts;
    let mut t = S;
    let mut acts = Vec::new();
    for _ in 0..40 {
        t += 250_000;
        acts.extend(b.handle(t, Event::Tick));
    }
    assert!(b.view(t).is_authority);
    assert!(b.view(t).rev.is_some(), "timeline lost");
    let acts = b.handle(t, Event::Command(UserCommand::Pause));
    assert!(
        writes(&acts)
            .iter()
            .any(|w| Intent::decode(&w.2).unwrap().paused),
        "{acts:?}"
    );
}

// ---------------------------------------------------------------------------------
// A restarted process with the same member key
// ---------------------------------------------------------------------------------

fn load_cmd(src: &str) -> Event {
    Event::Command(UserCommand::Load {
        kind: SourceKind::Url,
        source: src.as_bytes().to_vec(),
    })
}

fn load_sources(acts: &[Action]) -> Vec<Vec<u8>> {
    acts.iter()
        .filter_map(|a| match a {
            Action::Player(PlayerCmd::Load { source, .. }) => Some(source.clone()),
            _ => None,
        })
        .collect()
}

fn fresh_coordinator() -> Cowatch {
    let mut cfg = Config::new(k(A), 50);
    cfg.faults = Faults::default();
    let (mut a, _) = Cowatch::new(cfg, 0);
    a.handle(0, coord(0, A));
    a
}

/// A `Load` given before the snapshot is held; the previous run's record then arrives; the
/// load is applied above it and is what plays.
#[test]
fn a_load_before_the_snapshot_waits_and_lands_above_the_previous_runs_record() {
    let mut a = fresh_coordinator();
    let acts = a.handle(10_000, load_cmd("https://x.test/b"));
    assert!(writes(&acts).is_empty() && load_sources(&acts).is_empty(), "held");
    // The previous run's record (same key, revision 7) is in the snapshot.
    a.handle(20_000, room_rec(A, &intent(7, 30, 0)));
    let acts = a.handle(30_000, Event::Synced);
    assert_eq!(load_sources(&acts), vec![b"https://x.test/b".to_vec()]);
    let w = writes(&acts);
    assert_eq!(w.len(), 1);
    let rec = Intent::decode(&w[0].2).unwrap();
    assert!(rec.rev > 7, "written above the old run's revision: {}", rec.rev);
    assert_eq!(rec.source, b"https://x.test/b".to_vec());
}

/// The host never says `Synced`: the command is released after `sync_wait_ms`.
#[test]
fn held_commands_are_released_after_the_wait() {
    let mut a = fresh_coordinator();
    a.handle(10_000, load_cmd("https://x.test/b"));
    let acts = a.handle(2_900_000, Event::Tick);
    assert!(load_sources(&acts).is_empty());
    let acts = a.handle(3_100_000, Event::Tick);
    assert_eq!(load_sources(&acts), vec![b"https://x.test/b".to_vec()]);
}

/// The snapshot is late (after the wait): the previous run's record arrives after the
/// command was applied. Our latest is written again above it, and the old run does not win.
#[test]
fn a_previous_runs_record_arriving_after_our_write_does_not_win() {
    let mut a = fresh_coordinator();
    a.handle(0, Event::Synced);
    let acts = a.handle(10_000, load_cmd("https://x.test/b"));
    let first = Intent::decode(&writes(&acts)[0].2).unwrap();
    assert_eq!(first.rev, 1);
    // Old run's rev 7 arrives, then the echo of our rev 1.
    let acts = a.handle(20_000, room_rec(A, &intent(7, 30, 0)));
    let w = writes(&acts);
    assert_eq!(w.len(), 1, "restated");
    let again = Intent::decode(&w[0].2).unwrap();
    assert_eq!(again.rev, 8);
    assert_eq!(again.source, b"https://x.test/b".to_vec());
    let acts = a.handle(25_000, room_rec(A, &first));
    assert!(writes(&acts).is_empty(), "our own old echo changes nothing");
    assert_eq!(a.view(26_000).rev, Some(8));
    // A normal echo of our newest is not a predecessor.
    let acts = a.handle(30_000, room_rec(A, &again));
    assert!(writes(&acts).is_empty());
}

/// Request ids differ between restarts of the same key.
#[test]
fn request_ids_are_seeded_from_the_start_time() {
    let ids = |since: u64| -> u32 {
        let mut cfg = Config::new(k(B), since);
        cfg.faults = Faults::default();
        let (mut b, _) = Cowatch::new(cfg, 0);
        b.handle(0, Event::Synced);
        b.handle(0, cap(A, 1));
        b.handle(0, coord(0, A));
        let acts = b.handle(S, load_cmd("https://x.test/b"));
        match sends(&acts)[0].2.clone() {
            Control::Request { req_id, .. } => req_id,
            _ => panic!(),
        }
    };
    assert!(ids(5_000) < ids(65_000));
    assert!(ids(1_800_000_000_000) != ids(1_800_000_060_000));
}

/// A live twin with the same key (written after we started) is not a predecessor: no
/// restating over it, so two twins cannot flip the room back and forth forever.
#[test]
fn a_live_twins_record_is_not_restated_over() {
    let mut a = fresh_coordinator();
    a.handle(0, Event::Synced);
    a.handle(10_000, load_cmd("https://x.test/b"));
    let acts = a.handle(20_000, room_rec(A, &intent(9, 30, 5 * S)));
    assert!(writes(&acts).is_empty());
}

/// The predecessor's record is in the other scope than the one we wrote: still restated.
#[test]
fn a_predecessors_record_in_the_other_scope_is_restated_over() {
    let mut a = fresh_coordinator();
    a.handle(0, Event::Synced);
    a.handle(10_000, load_cmd("https://x.test/b"));
    let acts = a.handle(
        20_000,
        Event::Playback { room: false, writer: k(A), value: intent(9, 30, 0).encode() },
    );
    let w = writes(&acts);
    assert_eq!(w.len(), 1);
    assert_eq!(Intent::decode(&w[0].2).unwrap().source, b"https://x.test/b".to_vec());
}

/// Before the first snapshots the player is not told to load what the previous run left.
#[test]
fn the_player_is_not_driven_before_synced() {
    let mut cfg = Config::new(k(A), 50);
    cfg.faults = Faults::default();
    let (mut a, _) = Cowatch::new(cfg, 0);
    a.handle(0, coord(0, A));
    let acts = a.handle(10_000, room_rec(A, &intent(7, 30, 0)));
    assert!(load_sources(&acts).is_empty());
    let acts = a.handle(20_000, load_cmd("https://x.test/b"));
    assert!(load_sources(&acts).is_empty());
    let acts = a.handle(30_000, Event::Synced);
    assert_eq!(load_sources(&acts), vec![b"https://x.test/b".to_vec()]);
}
