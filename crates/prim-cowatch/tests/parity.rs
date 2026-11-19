//! Parity with the old drift correction, `project/media/playback.gd:436-466` (`correct`)
//! and `playback.gd:49-50` (`target_position`).
//!
//! Godot could not be run on the machine this was written on, so the oracle is a literal
//! transliteration of the GDScript below (same branch order, same constants, f64), plus a
//! hand-derived table. Where the Rust differs on purpose it is not compared: the rate
//! (the old code had none, so it is fixed to 1.0 here), the seek lead and the
//! verification/persistence seeks are `machine.rs` additions and are tested in sim.rs.
use prim_cowatch::correct::*;

struct Gd {
    speed: f64,
    seek: Option<f64>,
    paused: bool,
    drift: f64,
}

/// GDScript, line for line. `seek_threshold` is 0.75 for url/file sources
/// (0.25 for peer_file, which this crate does not have).
fn gd_correct(
    target: f64,
    position: f64,
    desired_paused: bool,
    awaiting_seek: bool,
    force: bool,
) -> Gd {
    let drift_seconds = target - position;
    let seek_threshold = 0.75;
    let mut out = Gd {
        speed: 1.0,
        seek: None,
        paused: desired_paused,
        drift: drift_seconds,
    };
    if awaiting_seek
        || (force && drift_seconds.abs() > 0.12)
        || drift_seconds.abs() > seek_threshold
    {
        out.seek = Some(target);
        out.speed = 1.0;
    } else {
        out.speed = if drift_seconds.abs() > 0.04 && !desired_paused {
            (1.0 + drift_seconds * 0.08).clamp(0.98, 1.02)
        } else {
            1.0
        };
    }
    out
}

fn rust(target: f64, position: f64, paused: bool, awaiting: bool, force: bool) -> Decision {
    correct(
        &Params::default(),
        &Inputs {
            target_s: target,
            position_s: position,
            rate: 1.0,
            desired_paused: paused,
            force,
            awaiting_seek: awaiting,
        },
    )
}

#[test]
fn matches_the_gdscript_on_a_grid() {
    let mut n = 0;
    // positions every 5 ms from -1.5 s to +1.5 s of drift, around a 10 s target
    for step in -300..=300 {
        let drift = step as f64 * 0.005;
        for paused in [false, true] {
            for awaiting in [false, true] {
                for force in [false, true] {
                    let (target, position) = (10.0, 10.0 - drift);
                    let g = gd_correct(target, position, paused, awaiting, force);
                    let r = rust(target, position, paused, awaiting, force);
                    assert_eq!(
                        r.seek_to_s, g.seek,
                        "seek at drift {drift} paused={paused} awaiting={awaiting} force={force}"
                    );
                    assert!(
                        (r.speed - g.speed).abs() < 1e-12,
                        "speed at drift {drift}: {} vs {}",
                        r.speed,
                        g.speed
                    );
                    assert_eq!(r.paused, g.paused);
                    assert!((r.drift_s - g.drift).abs() < 1e-12);
                    n += 1;
                }
            }
        }
    }
    assert_eq!(n, 601 * 8);
}

/// Hand-derived from reading the GDScript: (drift, paused, awaiting, force) -> (seeks, speed).
#[test]
fn hand_derived_cases() {
    let t = |drift: f64, paused, awaiting, force| {
        let d = rust(10.0, 10.0 - drift, paused, awaiting, force);
        (d.seek_to_s.is_some(), d.speed)
    };
    // Inside the dead band: untouched.
    assert_eq!(t(0.04, false, false, false), (false, 1.0));
    assert_eq!(t(-0.04, false, false, false), (false, 1.0));
    // 0.10 s behind: speed up 0.8 %.
    let (s, v) = t(0.10, false, false, false);
    assert!(!s && (v - 1.008).abs() < 1e-12);
    // The clamp: 0.5 s behind would be +4 %, capped at +2 %; ahead is capped at -2 %.
    assert_eq!(t(0.5, false, false, false), (false, 1.02));
    assert_eq!(t(-0.5, false, false, false), (false, 0.98));
    // Paused: never nudge the speed.
    assert_eq!(t(0.5, true, false, false), (false, 1.0));
    // Seek threshold is strict: exactly 0.75 nudges, a hair more seeks.
    assert_eq!(t(0.75, false, false, false), (false, 1.02));
    assert!(t(0.7501, false, false, false).0);
    assert!(t(-0.7501, false, false, false).0);
    // Forced (new revision, or after buffering): seeks above 0.12 s, not at or below it.
    assert!(t(0.1201, false, false, true).0);
    assert!(!t(0.12, false, false, true).0);
    // Awaiting the first seek: always seeks, even at zero drift.
    assert_eq!(t(0.0, false, true, false), (true, 1.0));
}

#[test]
fn target_position_matches_the_gdscript() {
    // GDScript: position + (now - stamp) when playing, position when paused, >= 0.
    assert_eq!(target_position(10.0, 100.0, 103.5, false, 1.0), 13.5);
    assert_eq!(target_position(10.0, 100.0, 103.5, true, 1.0), 10.0);
    assert_eq!(
        target_position(10.0, 100.0, 99.0, false, 1.0),
        10.0,
        "never runs backwards"
    );
    assert_eq!(target_position(0.0, 100.0, 100.0, false, 1.0), 0.0);
    // The new rate scales elapsed time.
    assert_eq!(target_position(10.0, 100.0, 104.0, false, 1.5), 16.0);
}
