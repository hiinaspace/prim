//! Scenario oracles on the real `room_core` simulator. Bounds are stated where asserted.
mod common;
use common::*;
use prim_cowatch::*;

/// Steady-state agreement bound between any two ready players, seconds. The seek
/// threshold is 0.75; the nudge keeps healthy pairs inside 0.15 on this network
/// (20..50 ms one-way, 10% datagram loss).
const STEADY: f64 = 0.15;
const SEEDS: std::ops::RangeInclusive<u64> = 1..=4;

fn start_all(w: &mut World, idxs: &[usize], duration: f64) {
    for &i in idxs {
        // Distinct clock origins (seconds apart), rate errors of ±~100 ppm on the machine
        // clock and ±~200 ppm on the media clock.
        let k = i as i64;
        w.start(
            i,
            duration,
            (k % 3 - 1) as f64 * 100.0,
            1_000_000 * (k + 1) * 7,
            ((k + 1) % 3 - 1) as f64 * 200.0,
        );
        w.run(100);
    }
}

fn max_skew_over(w: &mut World, ms: u64, idxs: &[usize]) -> f64 {
    let mut worst: f64 = 0.0;
    for _ in 0..ms / STEP_MS {
        w.step();
        if idxs.iter().all(|&i| w.n(i).p.playing_ready()) {
            worst = worst.max(w.skew(idxs));
        }
    }
    worst
}

fn settle(w: &mut World, idxs: &[usize]) {
    let ok = w.run_until(8_000, |w| w.all_playing(idxs) && w.skew(idxs) < 0.1);
    assert!(ok.is_some(), "never settled");
}

#[test]
fn baseline_syncs_and_stays_synced() {
    for seed in SEEDS {
        let mut w = World::new(seed, 3, net(10, 30));
        start_all(&mut w, &[0, 1, 2], 3600.0);
        w.command(1, url("https://example.test/a.mp4"));
        settle(&mut w, &[0, 1, 2]);
        let worst = max_skew_over(&mut w, 60_000, &[0, 1, 2]);
        println!("seed {seed}: worst steady skew {worst:.3}s");
        assert!(worst < STEADY, "seed {seed}: {worst}");
    }
}

#[test]
fn seek_and_pause_from_a_non_coordinator_take_effect_for_all() {
    for seed in SEEDS {
        let mut w = World::new(seed, 3, net(10, 30));
        start_all(&mut w, &[0, 1, 2], 3600.0);
        // Node 0 is the founder and so the coordinator; node 2 is a plain guest.
        w.command(2, url("https://example.test/a.mp4"));
        settle(&mut w, &[0, 1, 2]);
        w.run(5_000);

        w.command(2, UserCommand::SeekTo(1200.0));
        let t = w.run_until(3_000, |w| {
            (0..3).all(|i| (w.n(i).p.pos - 1200.0).abs() < 2.0) && w.skew(&[0, 1, 2]) < 0.2
        });
        println!("seed {seed}: seek landed for all after {t:?} ms");
        assert!(t.is_some(), "seek did not land for all");

        w.command(1, UserCommand::Pause);
        let t = w.run_until(3_000, |w| w.all_paused(&[0, 1, 2]));
        println!("seed {seed}: pause took {t:?} ms");
        assert!(t.is_some());
        let p0 = w.n(0).p.pos;
        w.run(3_000);
        assert!(
            (w.n(0).p.pos - p0).abs() < 0.05,
            "still moving while paused"
        );
        assert!(w.skew(&[0, 1, 2]) < 0.2);

        w.command(2, UserCommand::Play);
        let t = w.run_until(3_000, |w| w.all_playing(&[0, 1, 2]));
        assert!(t.is_some());
        w.command(1, UserCommand::SeekBy(-30.0));
        let t = w.run_until(3_000, |w| w.skew(&[0, 1, 2]) < 0.2 && w.n(0).p.pos < 1200.0);
        assert!(t.is_some(), "relative seek from a guest");
    }
}

#[test]
fn late_join_syncs_within_three_quarters_of_a_second_of_the_player_being_ready() {
    for seed in SEEDS {
        let mut w = World::new(seed, 4, net(10, 30));
        start_all(&mut w, &[0, 1, 2], 3600.0);
        w.command(1, url("https://example.test/a.mp4"));
        settle(&mut w, &[0, 1, 2]);
        w.run(20_000);
        w.start(3, 3600.0, 80.0, -4_000_000, 150.0);
        let t_ready = w
            .run_until(5_000, |w| w.n(3).first_ready_us.is_some())
            .expect("never ready");
        let ready_us = w.n(3).first_ready_us.unwrap();
        // Within 0.75 s of ready, the newcomer is within 0.25 s of the others.
        let mut synced_after = None;
        for k in 0..30 {
            if w.skew(&[0, 1, 2, 3]) < 0.25 && w.n(3).p.playing_ready() && !w.n(3).p.paused {
                synced_after = Some(w.sim.now_us - ready_us);
                break;
            }
            if k < 29 {
                w.step();
            }
        }
        println!(
            "seed {seed}: ready after {t_ready} ms, synced {:?} us after ready",
            synced_after
        );
        assert!(
            synced_after.is_some_and(|us| us <= 750_000),
            "seed {seed}: {synced_after:?}"
        );
        let worst = max_skew_over(&mut w, 15_000, &[0, 1, 2, 3]);
        assert!(worst < STEADY + 0.05, "{worst}");
    }
}

/// Playback goes on and can still be steered after `gone` disappears.
fn survive(seed: u64, silent: bool) -> (u64, u64) {
    let mut w = World::new(seed, 4, net(10, 30));
    start_all(&mut w, &[0, 1, 2, 3], 3600.0);
    w.command(2, url("https://example.test/a.mp4"));
    settle(&mut w, &[0, 1, 2, 3]);
    w.run(5_000);
    let rest = [1usize, 2, 3];
    let before: Vec<f64> = rest.iter().map(|&i| w.n(i).p.pos).collect();
    w.kill(0, silent); // node 0 is the coordinator
                       // Playback never stops and stays together.
    let mut worst: f64 = 0.0;
    let mut paused_ever = false;
    for _ in 0..(40_000 / STEP_MS) {
        w.step();
        worst = worst.max(w.skew(&rest));
        paused_ever |= rest.iter().any(|&i| w.n(i).p.paused);
    }
    let advanced: Vec<f64> = rest
        .iter()
        .enumerate()
        .map(|(k, &i)| w.n(i).p.pos - before[k])
        .collect();
    println!("seed {seed} silent={silent}: worst skew {worst:.3}, advanced {advanced:?}");
    assert!(!paused_ever, "playback paused during the takeover");
    assert!(worst < 0.5, "skew {worst}");
    assert!(
        advanced.iter().all(|a| *a > 38.0 && *a < 42.5),
        "{advanced:?}"
    );

    // Control is back: a pause and a seek from a guest take effect for the rest.
    let t0 = w.sim.now_us;
    w.command(3, UserCommand::Pause);
    let t_pause = w
        .run_until(40_000, |w| w.all_paused(&rest))
        .expect("pause never took effect");
    w.command(2, UserCommand::SeekTo(900.0));
    let t_seek = w
        .run_until(10_000, |w| {
            rest.iter().all(|&i| (w.n(i).p.pos - 900.0).abs() < 1.0)
        })
        .expect("seek never landed");
    let _ = t0;
    (t_pause, t_seek)
}

#[test]
fn coordinator_leaves_and_playback_continues_with_control_recovered() {
    for seed in SEEDS {
        let (p, s) = survive(seed, false);
        println!("seed {seed}: graceful leave: pause effective after {p} ms, seek after {s} ms");
        assert!(p <= 5_000, "control recovered too slowly: {p}");
    }
}

#[test]
fn coordinator_goes_unresponsive_and_playback_continues_with_control_recovered() {
    for seed in SEEDS {
        let (p, s) = survive(seed, true);
        println!("seed {seed}: crash: pause effective after {p} ms, seek after {s} ms");
        // Presence expiry (15 s) bounds how long a dead daemon still counts as present.
        assert!(p <= 20_000, "control recovered too slowly: {p}");
    }
}

#[test]
fn coordinator_daemon_with_no_player_is_bypassed() {
    for seed in SEEDS {
        let mut w = World::new(seed, 3, net(10, 30));
        // Node 0 (the founder, hence coordinator) runs a daemon but no player.
        start_all(&mut w, &[1, 2], 3600.0);
        w.command(2, url("https://example.test/a.mp4"));
        let t = w.run_until(8_000, |w| w.all_playing(&[1, 2]) && w.skew(&[1, 2]) < 0.1);
        println!(
            "seed {seed}: no-player coordinator, synced after {t:?} ms; authority {:?}",
            w.n(1).m.view(0).authority.map(|a| a.mode)
        );
        assert!(t.is_some());
        assert_eq!(w.n(1).m.view(0).authority.unwrap().mode, Mode::Acting);
        // The guest's pause reaches the acting leader.
        let guest = if w.n(1).m.is_authority() { 2 } else { 1 };
        w.command(guest, UserCommand::Pause);
        assert!(w.run_until(5_000, |w| w.all_paused(&[1, 2])).is_some());
        let worst = max_skew_over(&mut w, 20_000, &[1, 2]);
        assert!(worst < STEADY);
        // When the coordinator's user finally starts a player it takes over without a stutter.
        w.command(guest, UserCommand::Play);
        assert!(w.run_until(5_000, |w| w.all_playing(&[1, 2])).is_some());
        start_all(&mut w, &[0], 3600.0);
        w.run(10_000);
        let a = w.n(0).m.view(0).authority.unwrap();
        println!(
            "seed {seed}: after coordinator started a player: mode {:?}, holder is node0: {}",
            a.mode,
            a.key.holder == w.sim.key(0).0
        );
        assert_eq!(a.mode, Mode::Coordinator);
        assert!(
            w.all_playing(&[0, 1, 2]) && w.skew(&[0, 1, 2]) < STEADY + 0.05,
            "skew {}",
            w.skew(&[0, 1, 2])
        );
    }
}

#[test]
fn coordinators_player_crashed_but_daemon_alive_is_bypassed_via_no_channel() {
    for seed in SEEDS {
        let mut w = World::new(seed, 3, net(10, 30));
        start_all(&mut w, &[0, 1, 2], 3600.0);
        w.command(1, url("https://example.test/a.mp4"));
        settle(&mut w, &[0, 1, 2]);
        w.run(3_000);
        w.crash_player(0); // daemon and its cap record stay; the channel is gone
        w.run(2_000);
        w.command(2, UserCommand::Pause);
        let t = w.run_until(15_000, |w| w.all_paused(&[1, 2]));
        println!(
            "seed {seed}: pause after player crash took {t:?} ms; no_channel acks seen: {}",
            w.n(2)
                .send_log
                .iter()
                .filter(|x| x.1 == SendOutcome::NoChannel)
                .count()
        );
        assert!(t.is_some());
        assert!(
            w.n(2)
                .send_log
                .iter()
                .any(|x| x.1 == SendOutcome::NoChannel),
            "the NoChannel path was not exercised"
        );
        w.command(1, UserCommand::Play);
        assert!(w.run_until(5_000, |w| w.all_playing(&[1, 2])).is_some());
        assert!(max_skew_over(&mut w, 20_000, &[1, 2]) < STEADY);
    }
}

#[test]
fn clock_skew_and_rate_error_converge_and_stay_bounded() {
    for seed in SEEDS {
        let mut w = World::new(seed, 3, net(10, 30));
        // Machine clocks offset by seconds to hours, ±300 ppm; media clocks ±400 ppm.
        w.start(0, 7200.0, 300.0, 3_600_000_000, -400.0);
        w.start(1, 7200.0, -300.0, -2_000_000, 400.0);
        w.start(2, 7200.0, 0.0, 90_000_000, 250.0);
        w.command(1, url("https://example.test/a.mp4"));
        settle(&mut w, &[0, 1, 2]);
        let worst = max_skew_over(&mut w, 300_000, &[0, 1, 2]);
        println!("seed {seed}: 5 min, worst skew {worst:.3}s");
        // Stated bound: 0.15 s (probe error is at most half the path asymmetry, <= 15 ms here,
        // plus the nudge's own deadband of 40 ms and a media rate error of 400 ppm).
        assert!(worst < STEADY, "{worst}");
    }
}

#[test]
fn heavy_probe_loss_and_reordering() {
    for seed in SEEDS {
        let mut w = World::new(seed, 3, net(40, 400));
        start_all(&mut w, &[0, 1, 2], 3600.0);
        w.command(1, url("https://example.test/a.mp4"));
        let t = w.run_until(15_000, |w| {
            w.all_playing(&[0, 1, 2]) && w.skew(&[0, 1, 2]) < 0.2
        });
        assert!(t.is_some(), "seed {seed}");
        w.command(2, UserCommand::SeekTo(500.0));
        assert!(w
            .run_until(10_000, |w| (0..3)
                .all(|i| (w.n(i).p.pos - 500.0).abs() < 3.0))
            .is_some());
        w.command(2, UserCommand::Pause);
        assert!(w.run_until(10_000, |w| w.all_paused(&[0, 1, 2])).is_some());
        let worst = max_skew_over(&mut w, 60_000, &[0, 1, 2]);
        println!("seed {seed}: 40% datagram loss, 400 ms jitter: worst skew paused-then-steady {worst:.3}");
        assert!(worst < 0.25);
    }
}

// ---- faults shown red ---------------------------------------------------------------

/// A clean file reaches Finished within 2 s of its length.
fn clean_end(fault: bool) -> Option<u64> {
    let mut w = World::new(7, 2, net(10, 30));
    w.faults.always_reload = fault;
    start_all(&mut w, &[0, 1], 40.0);
    w.command(1, url("https://example.test/short.mp4"));
    w.run_until(5_000, |w| w.all_playing(&[0, 1]));
    // Both reach the end at about 40 s of media time.
    let start = w.sim.now_us;
    let pos0 = w.n(0).p.pos;
    let remaining_ms = ((40.0 - pos0) * 1000.0) as u64;
    let deadline = remaining_ms + 2_000;
    let ok = w.run_until(deadline, |w| {
        (0..2).all(|i| w.n(i).notices.iter().any(|n| n.1 == Notice::Finished))
    });
    let _ = start;
    ok
}

#[test]
fn a_clean_end_reaches_finished_within_two_seconds_of_the_length() {
    assert!(clean_end(false).is_some());
}

#[test]
fn red_always_reload_never_finishes() {
    // The injected bug turns every end into a reload; the oracle above must fail.
    assert!(
        clean_end(true).is_none(),
        "oracle did not catch the always-reload bug"
    );
}

#[test]
fn a_dropped_stream_reloads_at_the_room_position_and_is_never_finished() {
    for seed in SEEDS {
        let mut w = World::new(seed, 3, net(10, 30));
        start_all(&mut w, &[0, 1, 2], 3600.0);
        w.command(1, url("https://example.test/a.mp4"));
        settle(&mut w, &[0, 1, 2]);
        // Node 2's connection drops 100 s into a one-hour video, and the server refuses
        // the first reload as well.
        w.nm(2).p.drop_at = Some(w.n(2).p.pos + 5.0);
        w.nm(2).p.fail_loads = 1;
        w.run(60_000);
        let n2 = w.n(2);
        println!(
            "seed {seed}: notices {:?}, loads {}",
            n2.notices.iter().map(|n| n.1).collect::<Vec<_>>(),
            n2.p.loads
        );
        assert!(!n2.notices.iter().any(|n| n.1 == Notice::Finished));
        assert!(n2
            .notices
            .iter()
            .any(|n| matches!(n.1, Notice::Reloading { .. })));
        assert!(n2.p.loads >= 3, "initial + failed reload + good reload");
        assert!(
            w.all_playing(&[0, 1, 2]) && w.skew(&[0, 1, 2]) < STEADY,
            "skew {}",
            w.skew(&[0, 1, 2])
        );
    }
}

/// A player that silently ends up 0.5 s off is noticed and pulled back.
fn hidden_offset(fault: bool) -> f64 {
    let mut w = World::new(3, 3, net(10, 30));
    w.faults.no_correction = fault;
    start_all(&mut w, &[0, 1, 2], 3600.0);
    w.command(1, url("https://example.test/a.mp4"));
    if !fault {
        settle(&mut w, &[0, 1, 2]);
    } else {
        w.run(8_000);
    }
    w.run(5_000);
    w.nm(2).p.pos += 0.5; // nobody asked for this
    w.run(4_000);
    w.skew(&[0, 1, 2])
}

#[test]
fn a_silent_half_second_offset_is_detected_and_corrected() {
    assert!(hidden_offset(false) < 0.15);
}

#[test]
fn red_without_correction_the_half_second_offset_persists() {
    assert!(
        hidden_offset(true) > 0.4,
        "oracle did not see the uncorrected offset"
    );
}

/// Steer straight after the coordinator disappears, inside the gap before a new
/// coordinator exists.
fn steer_in_the_gap(seed: u64, silent: bool) -> (u64, bool) {
    let mut w = World::new(seed, 4, net(10, 30));
    start_all(&mut w, &[0, 1, 2, 3], 3600.0);
    w.command(2, url("https://example.test/a.mp4"));
    settle(&mut w, &[0, 1, 2, 3]);
    w.run(3_000);
    w.kill(0, silent);
    w.run(1_000);
    let rest = [1usize, 2, 3];
    w.command(3, UserCommand::Pause);
    let t = w
        .run_until(60_000, |w| w.all_paused(&rest))
        .expect("pause never took effect");
    // Everyone is on the same record afterwards.
    w.run(2_000);
    let revs: Vec<_> = rest.iter().map(|&i| w.n(i).m.view(0).rev).collect();
    let agree = revs.windows(2).all(|p| p[0] == p[1]);
    (t, agree)
}

#[test]
fn steering_inside_the_gap_after_the_coordinator_is_gone() {
    for seed in SEEDS {
        for silent in [false, true] {
            let (t, agree) = steer_in_the_gap(seed, silent);
            println!("seed {seed} silent={silent}: pause took effect {t} ms after being asked (1 s after the loss); revisions agree: {agree}");
            // Stated bound: a dead daemon stops counting as present at presence expiry
            // (15 s); an acting leader takes over then, or sooner on a clean leave.
            assert!(t <= if silent { 10_000 } else { 5_000 }, "{t}");
            assert!(agree);
        }
    }
}
