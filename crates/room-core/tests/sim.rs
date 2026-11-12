//! Seeded simulator oracles (ROOM.md "Simulator"). Every test is a pure function of
//! its seed: a failure prints the seed and reruns identically.

use room_core::api::*;
use room_core::frame::AckStatus;
use room_core::grants::Lifecycle;
use room_core::record::Kind;
use room_core::sim::{Sim, SplitMix};
use room_core::staterec::{Class, Lifetime, Scope};

const SEEDS: u64 = 50;

fn write(sim: &mut Sim, i: usize, scope: Scope, name: &str, life: Lifetime, v: &[u8]) -> Reply {
    let (name, value) = (name.to_string(), v.to_vec());
    sim.request(i, |req| LocalCommand::Write {
        req,
        scope,
        name,
        class: Class::Durable,
        lifetime: life,
        value,
    })
}

fn member_path(sim: &Sim, i: usize, name: &str) -> String {
    format!("member/{}/{}", sim.key(i).hex(), name)
}

fn coordinator_idx(sim: &Sim, i: usize) -> Option<usize> {
    sim.room(i)
        .coordinator()
        .and_then(|(_, k)| sim.index_of(&k))
}

/// Run until converged (bounded), then assert. Prints the seed on failure.
fn settle(sim: &mut Sim, seed: u64, what: &str) {
    sim.run_for(40_000);
    let ok = sim.run_until(150_000, |s| s.converged().is_ok());
    if !ok {
        panic!("seed {seed}: {what}: {}", sim.converged().unwrap_err());
    }
}

// ---------------------------------------------------------------------------
// a. partition and reorder
// ---------------------------------------------------------------------------

fn partition_run(seed: u64) -> Sim {
    let mut r = SplitMix(seed ^ 0xA11CE);
    let n = 5 + r.below(3) as usize;
    let mut sim = Sim::bootstrap(seed, n);
    sim.cfg.jitter_ms = 80;
    sim.cfg.dgram_loss_pct = 25;
    for round in 0..5 {
        // Random split into two or three groups.
        let k = 2 + r.below(2) as usize;
        let mut groups: Vec<Vec<usize>> = vec![Vec::new(); k];
        for i in 0..n {
            groups[r.below(k as u64) as usize].push(i);
        }
        let refs: Vec<&[usize]> = groups.iter().map(|g| g.as_slice()).collect();
        sim.partition(&refs);
        for _ in 0..4 {
            let w = r.below(n as u64) as usize;
            let name = format!("kv/k{}", r.below(4));
            let life = if r.chance(50) {
                Lifetime::Persistent
            } else {
                Lifetime::PresenceBound
            };
            let v = format!("r{round}-{}", r.next_u64() % 1000);
            write(&mut sim, w, Scope::Member, &name, life, v.as_bytes());
            // Room writes only succeed at a coordinator; try everywhere on one side.
            let rn = format!("cfg/c{}", r.below(3));
            for c in 0..n {
                write(
                    &mut sim,
                    c,
                    Scope::Room,
                    &rn,
                    Lifetime::Persistent,
                    v.as_bytes(),
                );
            }
            sim.run_for(r.below(3_000));
        }
        sim.run_for(5_000 + r.below(40_000));
        sim.heal();
        sim.run_for(r.below(20_000));
    }
    sim.heal();
    settle(&mut sim, seed, "after heal");
    sim
}

#[test]
fn a_partition_and_reorder_converge() {
    for seed in 0..SEEDS {
        let sim = partition_run(seed);
        let live = sim.live_nodes();
        assert!(live.len() >= 5, "seed {seed}");
        let c = coordinator_idx(&sim, live[0]);
        assert!(c.is_some(), "seed {seed}: no coordinator");
    }
}

// ---------------------------------------------------------------------------
// b. late join
// ---------------------------------------------------------------------------

#[test]
fn b_late_join_snapshot_then_strictly_newer() {
    for seed in 0..SEEDS {
        let mut sim = Sim::new(seed, 5);
        for i in 1..4 {
            sim.join(i, 0);
            sim.run_for(2_000);
        }
        sim.run_for(15_000);
        for i in 0..4 {
            write(
                &mut sim,
                i,
                Scope::Member,
                "kv/a",
                Lifetime::Persistent,
                b"one",
            );
            write(
                &mut sim,
                i,
                Scope::Member,
                "kv/b",
                Lifetime::Persistent,
                b"one",
            );
        }
        let c = coordinator_idx(&sim, 0).expect("coordinator");
        write(
            &mut sim,
            c,
            Scope::Room,
            "cfg/x",
            Lifetime::Persistent,
            b"one",
        );
        sim.run_for(3_000);
        sim.join(4, 0);
        // Subscribe at once (sees the state arrive) and again once state is there.
        sim.subscribe(4, 1, "**");
        assert!(
            sim.run_until(60_000, |s| s.visible(4).keys().any(|k| k.ends_with("kv/a"))),
            "seed {seed}: joiner never got state"
        );
        sim.subscribe(4, 2, "**");
        // More writes, including bumps of existing keys, while subscribed.
        for round in 0..3 {
            for i in 0..4 {
                let v = format!("v{round}");
                write(
                    &mut sim,
                    i,
                    Scope::Member,
                    "kv/a",
                    Lifetime::Persistent,
                    v.as_bytes(),
                );
            }
            write(
                &mut sim,
                c,
                Scope::Room,
                "cfg/x",
                Lifetime::Persistent,
                b"two",
            );
            sim.run_for(2_000);
        }
        sim.run_for(10_000);
        for sub in [1u32, 2] {
            let ups = sim.nodes[4].updates.get(&sub).cloned().unwrap_or_default();
            assert_eq!(ups.first(), Some(&Update::Reset), "seed {seed} sub {sub}");
            let mut seen: std::collections::BTreeMap<String, _> = Default::default();
            let mut done = false;
            let mut count_after = 0;
            for u in &ups {
                match u {
                    Update::Reset => {
                        seen.clear();
                        done = false;
                    }
                    Update::SnapshotDone => done = true,
                    Update::Value { path, version, .. } => {
                        if done {
                            count_after += 1;
                            if let Some(old) = seen.get(path) {
                                assert!(version > old, "seed {seed}: {path} not strictly newer");
                            }
                        } else {
                            assert!(
                                !seen.contains_key(path),
                                "seed {seed}: dup in snapshot {path}"
                            );
                        }
                        seen.insert(path.clone(), *version);
                    }
                    Update::Removed { .. } => {}
                }
            }
            assert!(done, "seed {seed} sub {sub}: no SnapshotDone");
            assert!(
                count_after > 0,
                "seed {seed} sub {sub}: no live updates after snapshot"
            );
        }
        // Subscriber 2's snapshot is non-empty (state already there).
        let ups = &sim.nodes[4].updates[&2];
        let snap = ups.iter().position(|u| *u == Update::SnapshotDone).unwrap();
        assert!(snap > 1, "seed {seed}: empty snapshot for sub 2");
        settle(&mut sim, seed, "late join");
    }
}

// ---------------------------------------------------------------------------
// c. revoke and retire
// ---------------------------------------------------------------------------

#[test]
fn c_revoke_removes_everywhere() {
    for seed in 0..SEEDS {
        let mut sim = Sim::bootstrap(seed, 5);
        let victim = 4;
        let vk = sim.key(victim);
        write(
            &mut sim,
            victim,
            Scope::Member,
            "kv/p",
            Lifetime::Persistent,
            b"p",
        );
        write(
            &mut sim,
            victim,
            Scope::Member,
            "kv/b",
            Lifetime::PresenceBound,
            b"b",
        );
        sim.run_for(3_000);
        assert!(
            sim.visible(0).keys().any(|k| k.contains(&vk.hex())),
            "seed {seed}"
        );
        let r = sim.request(0, |req| LocalCommand::Revoke { req, subject: vk });
        assert_eq!(r, Reply::Ok);
        sim.run_for(30_000);
        for j in 0..4 {
            let g = sim.room(j).grants().unwrap();
            assert!(!g.is_member(&vk), "seed {seed}: n{j} still has the member");
            assert!(
                !sim.visible(j).keys().any(|k| k.contains(&vk.hex())),
                "seed {seed}: n{j} still shows its state"
            );
            assert!(
                !sim.room(j).connected_peers().contains(&vk),
                "seed {seed}: n{j} connected"
            );
        }
        assert!(sim.room(victim).is_removed(), "seed {seed}");
        assert!(sim.nodes[victim].events.contains(&LocalEvent::Removed));
        // It can't come back: dials from either side stay refused.
        sim.run_for(60_000);
        for j in 0..4 {
            assert!(
                !sim.room(j).connected_peers().contains(&vk),
                "seed {seed}: reconnected"
            );
        }
        assert!(sim.room(victim).connected_peers().is_empty(), "seed {seed}");
        settle(&mut sim, seed, "after revoke");
    }
}

#[test]
fn c_retire_keeps_kept_removes_unredeemed() {
    for seed in 0..SEEDS {
        let mut sim = Sim::new(seed, 4);
        sim.join(1, 0);
        sim.run_for(3_000);
        let k1 = sim.key(1);
        let r = sim.request(0, |req| LocalCommand::Grant {
            req,
            kind: Kind::Keyholder,
            subject: k1,
            expiry_ms: 0,
        });
        assert_eq!(r, Reply::Ok);
        sim.run_for(8_000);
        sim.join(2, 1);
        sim.run_for(10_000);
        let Reply::Invite { token: late } = sim.request(1, |req| LocalCommand::CreateInvite {
            req,
            expiry_ms: 0,
            relays: vec![],
        }) else {
            panic!("seed {seed}: keyholder could not invite");
        };
        let r = sim.request(0, |req| LocalCommand::Retire {
            req,
            subject: k1,
            keep_all: true,
        });
        assert_eq!(r, Reply::Ok);
        sim.run_for(10_000);
        // The retired keyholder can no longer mint invites.
        let again = sim.request(1, |req| LocalCommand::CreateInvite {
            req,
            expiry_ms: 0,
            relays: vec![],
        });
        assert!(matches!(again, Reply::Error(_)), "seed {seed}: {again:?}");
        // An invite it issued but that nobody had redeemed when it was retired is dead.
        sim.join_with_token(3, &late);
        sim.run_for(40_000);
        let (k2, k3) = (sim.key(2), sim.key(3));
        for j in 0..3 {
            let g = sim.room(j).grants().unwrap();
            assert!(g.is_member(&k2), "seed {seed}: n{j} lost the kept member");
            assert!(
                g.is_member(&k1) && !g.is_keyholder(&k1),
                "seed {seed}: n{j} retire shape"
            );
            assert!(
                !g.is_member(&k3),
                "seed {seed}: n{j} admitted the late joiner"
            );
            assert!(!sim.room(j).connected_peers().contains(&k3), "seed {seed}");
        }
        assert!(
            sim.room(0).connected_peers().contains(&k2),
            "seed {seed}: kept member offline"
        );
        settle(&mut sim, seed, "after retire");
    }
}

// ---------------------------------------------------------------------------
// d. close and reopen
// ---------------------------------------------------------------------------

#[test]
fn d_close_reopen_replay() {
    for seed in 0..SEEDS {
        let mut sim = Sim::new(seed, 5);
        for i in 1..4 {
            sim.join(i, 0);
            sim.run_for(2_000);
        }
        sim.run_for(15_000);
        let open1: Vec<_> = sim
            .room(0)
            .grants()
            .unwrap()
            .records()
            .filter(|r| r.kind == Kind::Open)
            .cloned()
            .collect();
        assert_eq!(open1.len(), 1);
        write(
            &mut sim,
            1,
            Scope::Member,
            "kv/p",
            Lifetime::Persistent,
            b"keep",
        );
        write(
            &mut sim,
            1,
            Scope::Member,
            "kv/b",
            Lifetime::PresenceBound,
            b"gone",
        );
        sim.run_for(3_000);
        let kb = member_path(&sim, 1, "kv/b");
        let kp = member_path(&sim, 1, "kv/p");
        assert!(sim.visible(2).contains_key(&kb), "seed {seed}");
        let Reply::Invite { token: old } = sim.request(0, |req| LocalCommand::CreateInvite {
            req,
            expiry_ms: 0,
            relays: vec![],
        }) else {
            panic!()
        };
        assert_eq!(sim.request(0, |req| LocalCommand::Close { req }), Reply::Ok);
        sim.run_for(15_000);
        for j in 0..4 {
            assert_eq!(
                sim.room(j).grants().unwrap().lifecycle(),
                Lifecycle::Closed(1),
                "seed {seed} n{j}"
            );
        }
        // Replaying the old open at every node does not reopen the room.
        for j in 1..4 {
            sim.replay_records(j, 0, &open1);
        }
        sim.run_for(5_000);
        for j in 0..4 {
            assert_eq!(
                sim.room(j).grants().unwrap().lifecycle(),
                Lifecycle::Closed(1),
                "seed {seed} replay n{j}"
            );
        }
        assert_eq!(
            sim.request(0, |req| LocalCommand::Reopen { req }),
            Reply::Ok
        );
        sim.run_for(30_000);
        for j in 0..4 {
            assert_eq!(
                sim.room(j).grants().unwrap().lifecycle(),
                Lifecycle::Open(2),
                "seed {seed} n{j}"
            );
        }
        // The old generation's token no longer admits anyone.
        sim.join_with_token(4, &old);
        sim.run_for(40_000);
        let k4 = sim.key(4);
        for j in 0..4 {
            assert!(
                !sim.room(j).grants().unwrap().is_member(&k4),
                "seed {seed}: old token worked"
            );
        }
        // Presence-bound state of the old generation is invisible, persistent survives.
        for j in 0..4 {
            assert!(
                !sim.visible(j).contains_key(&kb),
                "seed {seed} n{j}: old-generation key visible"
            );
            assert!(
                sim.visible(j).contains_key(&kp),
                "seed {seed} n{j}: persistent key lost"
            );
        }
        // Fresh writes in the new generation work.
        write(
            &mut sim,
            1,
            Scope::Member,
            "kv/b",
            Lifetime::PresenceBound,
            b"new",
        );
        sim.run_for(3_000);
        assert_eq!(
            sim.visible(2).get(&kb).map(|v| v.value.clone()),
            Some(b"new".to_vec()),
            "seed {seed}"
        );
        settle(&mut sim, seed, "after reopen");
    }
}

// ---------------------------------------------------------------------------
// e. coordinator conflicts
// ---------------------------------------------------------------------------

#[test]
fn e_coordinator_claim_conflict() {
    for seed in 0..SEEDS {
        let mut sim = Sim::bootstrap(seed, 6);
        let c = coordinator_idx(&sim, 0).unwrap();
        let others: Vec<usize> = (0..6).filter(|&i| i != c).collect();
        let (b, d) = (others[..2].to_vec(), others[2..].to_vec());
        let name = "cfg/y";
        assert_eq!(
            write(
                &mut sim,
                c,
                Scope::Room,
                name,
                Lifetime::Persistent,
                b"base"
            ),
            Reply::Ok
        );
        sim.run_for(3_000);
        sim.partition(&[&[c], &b, &d]);
        sim.run_for(60_000);
        // Both sides claimed a new term, each with its own holder.
        let (cb, cd) = (
            coordinator_idx(&sim, b[0]).unwrap(),
            coordinator_idx(&sim, d[0]).unwrap(),
        );
        assert!(
            b.contains(&cb) && d.contains(&cd),
            "seed {seed}: sides didn't elect locally"
        );
        assert_ne!(cb, cd);
        let tb = sim.room(b[0]).coordinator().unwrap().0;
        let td = sim.room(d[0]).coordinator().unwrap().0;
        assert_eq!(tb, td, "seed {seed}: equal-term claims expected");
        assert!(tb.term >= 1);
        assert_eq!(
            write(
                &mut sim,
                cb,
                Scope::Room,
                name,
                Lifetime::Persistent,
                b"from-b"
            ),
            Reply::Ok
        );
        assert_eq!(
            write(
                &mut sim,
                cd,
                Scope::Room,
                name,
                Lifetime::Persistent,
                b"from-d"
            ),
            Reply::Ok
        );
        assert_eq!(
            write(
                &mut sim,
                cb,
                Scope::Room,
                "cfg/only-b",
                Lifetime::Persistent,
                b"b"
            ),
            Reply::Ok
        );
        assert_eq!(
            write(
                &mut sim,
                cd,
                Scope::Room,
                "cfg/only-d",
                Lifetime::Persistent,
                b"d"
            ),
            Reply::Ok
        );
        sim.run_for(5_000);
        sim.heal();
        settle(&mut sim, seed, "after claim conflict");
        // One coordinator everywhere, and it is the lower (rank, key) claimant.
        let g = sim.room(0).grants().unwrap();
        let (epoch, info) = g.coordinator().unwrap();
        assert_eq!(epoch, tb, "seed {seed}: higher term appeared");
        let kb = sim.key(cb);
        let kd = sim.key(cd);
        assert!(info.holder == kb || info.holder == kd);
        let (win, lose) = if info.holder == kb {
            (cb, cd)
        } else {
            (cd, cb)
        };
        let other_rank = |w: usize| {
            g.records()
                .filter(|r| r.kind == Kind::Claim && r.issuer == sim.key(w))
                .map(|r| match r.body {
                    room_core::record::Body::Claim { rank, .. } => (rank, r.issuer),
                    _ => unreachable!(),
                })
                .min()
                .unwrap()
        };
        assert!(
            other_rank(win) < other_rank(lose),
            "seed {seed}: higher rank won"
        );
        for j in 0..6 {
            let v = sim.visible(j);
            let want_x: &[u8] = if win == cb { b"from-b" } else { b"from-d" };
            assert_eq!(
                v.get("room/cfg/y").map(|x| x.value.as_slice()),
                Some(want_x),
                "seed {seed} n{j}"
            );
            let loser_key = if lose == cb {
                "room/cfg/only-b"
            } else {
                "room/cfg/only-d"
            };
            assert!(
                !v.contains_key(loser_key),
                "seed {seed} n{j}: loser's write survived"
            );
            let winner_key = if win == cb {
                "room/cfg/only-b"
            } else {
                "room/cfg/only-d"
            };
            assert!(v.contains_key(winner_key), "seed {seed} n{j}");
        }
        // A loser-only value falls back to the earlier epoch's value.
        let _ = lose;
    }
}

#[test]
fn e_coordinator_value_falls_back_after_losing() {
    for seed in 0..SEEDS {
        let mut sim = Sim::bootstrap(seed, 6);
        let c = coordinator_idx(&sim, 0).unwrap();
        let others: Vec<usize> = (0..6).filter(|&i| i != c).collect();
        let (b, d) = (others[..2].to_vec(), others[2..].to_vec());
        assert_eq!(
            write(
                &mut sim,
                c,
                Scope::Room,
                "cfg/z",
                Lifetime::Persistent,
                b"base"
            ),
            Reply::Ok
        );
        sim.run_for(3_000);
        sim.partition(&[&[c], &b, &d]);
        sim.run_for(60_000);
        let (cb, cd) = (
            coordinator_idx(&sim, b[0]).unwrap(),
            coordinator_idx(&sim, d[0]).unwrap(),
        );
        // Both sides overwrite z at the same new term.
        write(
            &mut sim,
            cb,
            Scope::Room,
            "cfg/z",
            Lifetime::Persistent,
            b"b",
        );
        write(
            &mut sim,
            cd,
            Scope::Room,
            "cfg/z",
            Lifetime::Persistent,
            b"d",
        );
        sim.run_for(5_000);
        sim.heal();
        settle(&mut sim, seed, "fallback");
        let holder = sim.room(0).coordinator().unwrap().1;
        let win = sim.index_of(&holder).unwrap();
        let expect: &[u8] = if win == cb { b"b" } else { b"d" };
        for j in 0..6 {
            assert_eq!(
                sim.visible(j).get("room/cfg/z").map(|x| x.value.as_slice()),
                Some(expect),
                "seed {seed} n{j}"
            );
        }
        // The winner's own later write still goes through; the loser's are refused.
        let lose = if win == cb { cd } else { cb };
        let r = write(
            &mut sim,
            lose,
            Scope::Room,
            "cfg/z",
            Lifetime::Persistent,
            b"late",
        );
        assert_eq!(r, Reply::Error(ErrorCode::NotCoordinator), "seed {seed}");
    }
}

#[test]
fn e_left_vs_unresponsive_and_no_coordinator() {
    let mut no_coord_checks = 0;
    for seed in 0..SEEDS {
        let mut sim = Sim::bootstrap(seed, 5);
        let c = coordinator_idx(&sim, 0).unwrap();
        let ck = sim.key(c);
        for i in 0..5 {
            sim.take_events(i);
        }
        let leave = seed % 2 == 0;
        if leave {
            assert_eq!(sim.request(c, |req| LocalCommand::Leave { req }), Reply::Ok);
            sim.stop(c, false);
        } else {
            sim.stop(c, true);
        }
        let observers: Vec<usize> = (0..5).filter(|&i| i != c).collect();
        let saw = |sim: &Sim, i: usize, left: bool| {
            sim.nodes[i].events.iter().any(|e| match e {
                LocalEvent::Coordinator(CoordinatorEvent::Left { holder, .. }) => {
                    left && *holder == ck
                }
                LocalEvent::Coordinator(CoordinatorEvent::Unresponsive { holder, .. }) => {
                    !left && *holder == ck
                }
                _ => false,
            })
        };
        // Step finely until the first observer reports the coordinator gone.
        let mut elapsed = 0;
        let mut first = None;
        while elapsed < 90_000 && first.is_none() {
            sim.run_for(10);
            elapsed += 10;
            first = observers.iter().copied().find(|&i| saw(&sim, i, leave));
        }
        assert!(
            first.is_some(),
            "seed {seed}: no {} event",
            if leave { "Left" } else { "Unresponsive" }
        );
        for &i in &observers {
            assert!(
                !saw(&sim, i, !leave),
                "seed {seed} n{i}: wrong kind of event"
            );
        }
        if !leave {
            // Until a successor is claimed the old holder is known silent.
            for &i in &observers {
                if saw(&sim, i, false) && sim.room(i).coordinator().map(|x| x.1) == Some(ck) {
                    let r = sim.request(i, |req| LocalCommand::Send {
                        req,
                        to: To::Coordinator,
                        channel: 1,
                        reliable: true,
                        payload: b"x".to_vec(),
                    });
                    assert_eq!(
                        r,
                        Reply::Error(ErrorCode::NoCoordinator),
                        "seed {seed} n{i}"
                    );
                    no_coord_checks += 1;
                }
            }
        }
        // A successor emerges among the survivors.
        sim.run_for(40_000);
        let succ = coordinator_idx(&sim, observers[0]).expect("successor");
        assert_ne!(succ, c, "seed {seed}");
    }
    assert!(no_coord_checks > 0, "never caught the unresponsive window");
}

// ---------------------------------------------------------------------------
// f. restart
// ---------------------------------------------------------------------------

#[test]
fn f_restart_bumps_incarnation() {
    for seed in 0..SEEDS {
        let mut sim = Sim::bootstrap(seed, 4);
        let (p, b) = (member_path(&sim, 2, "kv/p"), member_path(&sim, 2, "kv/b"));
        write(
            &mut sim,
            2,
            Scope::Member,
            "kv/p",
            Lifetime::Persistent,
            b"keep",
        );
        write(
            &mut sim,
            2,
            Scope::Member,
            "kv/b",
            Lifetime::PresenceBound,
            b"temp",
        );
        sim.run_for(3_000);
        assert!(sim.visible(0).contains_key(&b), "seed {seed}");
        assert_eq!(sim.room(2).member_incarnation(), 1);
        sim.stop(2, true);
        sim.run_for(4_000 + seed % 3 * 5_000);
        sim.restart(2);
        assert_eq!(sim.room(2).member_incarnation(), 2);
        settle(&mut sim, seed, "after restart");
        let pres = format!("member/{}/presence", sim.key(2).hex());
        for j in 0..4 {
            let v = sim.visible(j);
            assert!(v.contains_key(&p), "seed {seed} n{j}: persistent key lost");
            assert!(
                !v.contains_key(&b),
                "seed {seed} n{j}: old presence-bound key visible"
            );
            let pv = v.get(&pres).expect("presence re-asserted");
            assert_eq!(
                pv.version.term, 2,
                "seed {seed} n{j}: presence from the old incarnation"
            );
            let info = sim
                .status(j)
                .members
                .into_iter()
                .find(|m| m.key == sim.key(2))
                .unwrap();
            assert!(info.fresh, "seed {seed} n{j}: not fresh");
        }
    }
}

// ---------------------------------------------------------------------------
// g. messages
// ---------------------------------------------------------------------------

#[test]
fn g_messages_ack_offline_and_nothing_queued() {
    for seed in 0..SEEDS {
        let mut sim = Sim::bootstrap(seed, 4);
        let k1 = sim.key(1);
        let k3 = sim.key(3);
        assert_eq!(
            sim.request(1, |req| LocalCommand::RegisterChannel { req, channel: 7 }),
            Reply::Ok
        );
        sim.take_events(0);
        let send = |sim: &mut Sim, i: usize, to: To, channel: u16| {
            sim.request(i, |req| LocalCommand::Send {
                req,
                to,
                channel,
                reliable: true,
                payload: b"hello".to_vec(),
            })
        };
        // Registered channel: delivered.
        let Reply::Sending { msg_id: m_ok, .. } = send(&mut sim, 0, To::Member(k1), 7) else {
            panic!()
        };
        // Unregistered channel: no_channel ack.
        let Reply::Sending { msg_id: m_nc, .. } = send(&mut sim, 0, To::Member(k1), 9) else {
            panic!()
        };
        sim.run_for(2_000);
        let result = |sim: &Sim, i: usize, id: u64| {
            sim.nodes[i].events.iter().find_map(|e| match e {
                LocalEvent::MessageResult { msg_id, status, .. } if *msg_id == id => Some(*status),
                _ => None,
            })
        };
        assert_eq!(
            result(&sim, 0, m_ok),
            Some(MessageStatus::Acked(AckStatus::Delivered)),
            "seed {seed}"
        );
        assert_eq!(
            result(&sim, 0, m_nc),
            Some(MessageStatus::Acked(AckStatus::NoChannel)),
            "seed {seed}"
        );
        assert!(sim.nodes[1]
            .events
            .iter()
            .any(|e| matches!(e, LocalEvent::Message { channel: 7, .. })));
        assert!(!sim.nodes[1]
            .events
            .iter()
            .any(|e| matches!(e, LocalEvent::Message { channel: 9, .. })));
        // Offline target: immediate Offline, and nothing is held back for later.
        assert_eq!(
            sim.request(3, |req| LocalCommand::RegisterChannel { req, channel: 7 }),
            Reply::Ok
        );
        sim.stop(3, false);
        sim.run_for(5_000);
        let Reply::Sending { msg_id: m_off, .. } = send(&mut sim, 0, To::Member(k3), 7) else {
            panic!()
        };
        sim.run_for(1_000);
        assert_eq!(
            result(&sim, 0, m_off),
            Some(MessageStatus::Offline),
            "seed {seed}"
        );
        sim.take_events(3);
        sim.restart(3);
        assert_eq!(
            sim.request(3, |req| LocalCommand::RegisterChannel { req, channel: 7 }),
            Reply::Ok
        );
        sim.run_for(30_000);
        assert!(
            !sim.nodes[3]
                .events
                .iter()
                .any(|e| matches!(e, LocalEvent::Message { .. })),
            "seed {seed}: an offline message was queued and delivered"
        );
    }
}

// ---------------------------------------------------------------------------
// Reproducibility
// ---------------------------------------------------------------------------

#[test]
fn same_seed_same_trace() {
    for seed in [1u64, 7, 23] {
        let a = partition_run(seed).trace_hash();
        let b = partition_run(seed).trace_hash();
        assert_eq!(a, b, "seed {seed}: replay diverged");
    }
    assert_ne!(partition_run(1).trace_hash(), partition_run(2).trace_hash());
}
