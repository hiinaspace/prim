//! prim-voice against a mock daemon. These run in real time (NetEq and the mixer are
//! paced by the wall clock), so each takes a few seconds.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use prim_voice::clock::{mono_us, sleep_until};
use prim_voice::seats::SeatSel;
use prim_voice::{ArrivalPolicy, CaptureSpec, Client, Config, HandshakeError, OutputSpec};
use room_core::rt::{refuse, Pose, Role, RtMsg};
use room_core::voice::{voice_flags, voice_state};

fn capture_cfg(buf: &Capture) -> Config {
    Config {
        output: OutputSpec::Capture(buf.clone()),
        ..Config::default()
    }
}

fn frames(buf: &Capture) -> usize {
    buf.lock().unwrap().len() / 2
}

#[test]
fn handshake_hello_and_refusal() {
    let (mock, conn) = Mock::spawn(key(9), false);
    let cfg = Config {
        takeover: true,
        capture: CaptureSpec::Tone(440.0),
        incarnation: Some(0xabcd),
        ..Config::default()
    };
    let c = Client::start(conn, cfg).unwrap();
    match mock.hello.lock().unwrap().clone().unwrap() {
        RtMsg::Hello {
            role,
            takeover,
            incarnation,
            flags,
            proto,
        } => {
            assert_eq!(role, Role::Voice);
            assert!(takeover);
            assert_eq!(incarnation, 0xabcd);
            assert_eq!(flags, voice_state::LISTENING | voice_state::MIC);
            assert_eq!(proto, 1);
        }
        m => panic!("{m:?}"),
    }
    assert_eq!(c.self_key(), key(9));
    c.stop();

    let (_m2, conn) = Mock::spawn(key(9), true);
    let e = Client::start(conn, Config::default())
        .err()
        .expect("refused");
    let h = e
        .chain()
        .find_map(|c| c.downcast_ref::<HandshakeError>())
        .unwrap();
    assert!(
        matches!(h, HandshakeError::Refused(r) if *r == refuse::BUSY),
        "{h}"
    );
}

#[test]
fn idle_client_sends_keepalives_and_random_incarnations_are_nonzero_and_fresh() {
    let (mock, conn) = Mock::spawn(key(9), false);
    let c = Client::start(conn, Config::default()).unwrap();
    let inc = c.incarnation();
    assert_ne!(inc, 0);
    std::thread::sleep(Duration::from_millis(1100));
    let n = mock
        .drain()
        .iter()
        .filter(|(_, m)| *m == RtMsg::Keepalive)
        .count();
    assert!((3..=5).contains(&n), "keepalives in 1.1 s: {n}");
    c.stop();
    let (_m, conn) = Mock::spawn(key(9), false);
    let c2 = Client::start(conn, Config::default()).unwrap();
    assert_ne!(c2.incarnation(), inc);
    c2.stop();
}

#[test]
fn send_path_makes_20ms_opus_with_stream_start_on_the_first_five() {
    let (mock, conn) = Mock::spawn(key(9), false);
    let c = Client::start(
        conn,
        Config {
            capture: CaptureSpec::ToneAuto,
            incarnation: Some(77),
            ..Config::default()
        },
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(900));
    c.stop();
    let outs: Vec<_> = mock
        .drain()
        .into_iter()
        .filter_map(|(t, m)| match m {
            RtMsg::VoiceOut { sent_mono_us, env } => Some((t, sent_mono_us, env)),
            _ => None,
        })
        .collect();
    assert!(outs.len() >= 35, "{} packets in 0.9 s", outs.len());
    for (i, (t, sent, e)) in outs.iter().enumerate() {
        assert_eq!(e.incarnation, 77);
        assert_eq!(e.seq, outs[0].2.seq.wrapping_add(i as u16));
        assert_eq!(
            e.timestamp,
            outs[0].2.timestamp.wrapping_add(960 * i as u32)
        );
        assert_eq!(
            e.flags & voice_flags::STREAM_START != 0,
            i < 5,
            "packet {i}"
        );
        assert!(
            *sent <= *t && t - sent < 20_000,
            "sent stamp is CLOCK_MONOTONIC at the write"
        );
    }
    assert!(outs[0].2.flags & voice_flags::TALKSPURT_START != 0);
}

#[test]
fn two_talkers_render_non_silent_stereo_placed_on_their_seats_at_the_matched_level() {
    let buf: Capture = Arc::new(Mutex::new(Vec::new()));
    let (mock, conn) = Mock::spawn(key(9), false);
    let mut cfg = capture_cfg(&buf);
    cfg.seats = vec![
        (SeatSel::KeyPrefix("0101".into()), 90.0),
        (SeatSel::KeyPrefix("0202".into()), -90.0),
    ];
    let c = Client::start(conn, cfg).unwrap();
    let (fa, fb) = (700.0, 1100.0);
    let a = play(
        mock.conn.clone(),
        key(1),
        11,
        encode_tone(fa, 0.3, 160, 0, 0),
        0,
        0,
        1,
    );
    let b = play(
        mock.conn.clone(),
        key(2),
        22,
        encode_tone(fb, 0.3, 160, 0, 0),
        0,
        0,
        2,
    );
    a.join().unwrap();
    b.join().unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let stats = c.stop();
    let out = buf.lock().unwrap().clone();
    let n = out.len() / 2;
    // The window 1.5 s .. 2.5 s into the output: both talkers are well established.
    let (al, ar) = amp_l_r(&out, 72_000, 48_000, fa);
    let (bl, br) = amp_l_r(&out, 72_000, 48_000, fb);
    eprintln!("A(+90deg) L={al:.4} R={ar:.4}   B(-90deg) L={bl:.4} R={br:.4}   ({n} frames)");
    assert!(ar > 0.02 && bl > 0.02, "non-silent");
    assert!(
        ar > 1.5 * al,
        "+90 deg is louder in the right ear: L={al} R={ar}"
    );
    assert!(
        bl > 1.5 * br,
        "-90 deg is louder in the left ear: L={bl} R={br}"
    );

    // Level: a 0.3 sine rendered straight through prim-hrtf at the same seat, with
    // the gain written out here (150 % law at 1.5 m, then -4.7 dB).
    let level_matched = 1.5 * 10f32.powf(-4.7 / 20.0);
    let reference = |gain: f32| -> f32 {
        let hrtf = prim_hrtf::Hrtf::from_bytes(prim_voice::mixer::HRTF_BYTES).unwrap();
        let mut r = prim_hrtf::Renderer::new(hrtf, prim_hrtf::RendererConfig::default()).unwrap();
        let (mut l, mut rr) = (vec![0f32; 480], vec![0f32; 480]);
        let mut right = Vec::new();
        for blk in 0..200 {
            let x: Vec<f32> = (0..480)
                .map(|k| {
                    0.3 * (std::f64::consts::TAU * fa as f64 * (blk * 480 + k) as f64 / 48000.0)
                        .sin() as f32
                })
                .collect();
            l.fill(0.0);
            rr.fill(0.0);
            r.render_source(
                0,
                &x,
                prim_hrtf::SourceParams {
                    direction: prim_hrtf::direction_from_angles(90.0, 0.0),
                    gain,
                },
                &mut l,
                &mut rr,
            );
            if blk >= 100 {
                right.extend_from_slice(&rr);
            }
        }
        prim_voice::tone::goertzel_amp(&right, fa, 48000.0)
    };
    let want = reference(level_matched);
    let unmatched = reference(1.5);
    let db = 20.0 * (ar / want).log10();
    eprintln!("A right ear: measured {ar:.4}, prim-hrtf at matched gain {want:.4} ({db:+.2} dB), unmatched would be {unmatched:.4}");
    assert!(
        db.abs() < 2.5,
        "level_match applied: {db:+.2} dB from the matched reference"
    );
    assert!(
        20.0 * (ar / unmatched).log10() < -2.5,
        "and clearly not the unmatched level"
    );
    assert_eq!(
        stats["underruns"].as_u64().unwrap(),
        0,
        "no output underruns"
    );
}

#[test]
fn restart_fencing_drops_late_old_incarnation_and_the_old_tone_is_gone() {
    let buf: Capture = Arc::new(Mutex::new(Vec::new()));
    let (mock, conn) = Mock::spawn(key(9), false);
    let mut cfg = capture_cfg(&buf);
    cfg.stats_tone = true;
    cfg.seats = vec![(SeatSel::KeyPrefix("0101".into()), 0.0)];
    let c = Client::start(conn, cfg).unwrap();
    let (f1, f2) = (prim_voice::tone::auto_hz(1), prim_voice::tone::auto_hz(2));
    let p1 = encode_tone(f1, 0.3, 75, 0, 0);
    let p2 = encode_tone(f2, 0.3, 100, 0, 0);
    // Incarnation 1 plays 70 packets; the last five are held back, as a network
    // would delay them past the restart.
    let (head, held) = p1.split_at(70);
    play(mock.conn.clone(), key(1), 1, head.to_vec(), 0, 0, 3)
        .join()
        .unwrap();
    std::thread::sleep(Duration::from_millis(40));
    let switch_frame = frames(&buf);
    let t2 = play(mock.conn.clone(), key(1), 2, p2, 0, 0, 4);
    std::thread::sleep(Duration::from_millis(30));
    for (i, p) in held.iter().enumerate() {
        mock.voice_in(key(1), mono_us(), env(p, 1, 70 + i as u64));
    }
    t2.join().unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let stale = c.counter("fence_stale");
    let new_streams = c.counter("fence_new_stream");
    let stats = c.stop();
    let out = buf.lock().unwrap().clone();
    // Before the switch the old tone is there; from 0.5 s after, only the new one.
    let (pre1, pre1r) = amp_l_r(&out, switch_frame - 24_000, 24_000, f1);
    let (o1l, o1r) = amp_l_r(&out, switch_frame + 24_000, 48_000, f1);
    let (o2l, o2r) = amp_l_r(&out, switch_frame + 24_000, 48_000, f2);
    eprintln!(
        "before switch f1 L={pre1:.4} R={pre1r:.4}; after: f1 L={o1l:.5} R={o1r:.5}, f2 L={o2l:.4} R={o2r:.4}; stale={stale} new_stream={new_streams}"
    );
    assert!(pre1 > 0.02, "old tone audible before the restart");
    assert!(o2l > 0.02 && o2r > 0.02, "new tone audible after");
    assert!(
        o1l < 0.03 * o2l && o1r < 0.03 * o2r,
        "no trace of the old tone after the switch"
    );
    assert_eq!(
        stale, 5,
        "the five delayed incarnation-1 packets are dropped as stale"
    );
    assert_eq!(new_streams, 2);
    // The tone stats agree: in the last full second that still carries the new tone,
    // the old tone's energy is nil.
    let secs = stats["tone_seconds"].as_array().unwrap();
    let last = secs
        .iter()
        .rev()
        .find(|s| {
            s["tones"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["incarnation"] == 2 && t["l"].as_f64().unwrap() > 0.02)
        })
        .expect("a second with the new tone");
    let amp = |inc: u64| {
        last["tones"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["incarnation"] == inc)
            .map(|t| t["l"].as_f64().unwrap())
            .unwrap()
    };
    eprintln!(
        "tone stats, last second with the new tone: old {:.5} new {:.4}",
        amp(1),
        amp(2)
    );
    assert!(amp(1) < 0.03 * amp(2));
}

#[derive(Default, Debug)]
struct Delay {
    target: f64,
    preferred: f64,
    buffer: f64,
    accel_per_s: f64,
    expand_per_s: f64,
    preempt_per_s: f64,
}

/// Mean NetEq delay figures over the last 5 s of an 8 s call whose packets are due
/// 20 ms apart, reach the daemon up to `net_us` late (it stamps them then), and reach
/// the client up to `ipc_us` after that.
fn neteq_delay(arrival: ArrivalPolicy, net_us: u64, ipc_us: u64, seed: u64) -> Delay {
    let (mock, conn) = Mock::spawn(key(9), false);
    let cfg = Config {
        arrival,
        seats: vec![(SeatSel::KeyPrefix("0101".into()), 0.0)],
        ..Config::default()
    };
    let c = Client::start(conn, cfg).unwrap();
    let t = play_with_net(
        mock.conn.clone(),
        key(1),
        5,
        encode_tone(500.0, 0.3, 400, 0, 0),
        0,
        net_us,
        ipc_us,
        seed,
    );
    let t0 = mono_us();
    let mut d = Delay::default();
    let mut n = 0.0;
    while mono_us() - t0 < 7_800_000 {
        sleep_until(mono_us() + 100_000);
        if mono_us() - t0 > 3_000_000 {
            if let Some(s) = c.peer_receiver_stats(&key(1)) {
                d.target += s.target_delay_ms as f64;
                d.preferred += s.preferred_buffer_size_ms as f64;
                d.buffer += s.current_buffer_size_ms as f64;
                d.accel_per_s += s.accelerate_per_sec as f64;
                d.expand_per_s += s.expand_per_sec as f64;
                d.preempt_per_s += s.preemptive_expand_per_sec as f64;
                n += 1.0;
            }
        }
    }
    t.join().unwrap();
    c.stop();
    drop(mock);
    d.target /= n;
    d.preferred /= n;
    d.buffer /= n;
    d.accel_per_s /= n;
    d.expand_per_s /= n;
    d.preempt_per_s /= n;
    d
}

#[test]
fn neteq_arrival_is_the_daemon_stamp_so_ipc_jitter_does_not_grow_the_buffer() {
    // NetEq quantises its target in 20 ms steps, so the effect shows near a step: with
    // 24 or 26 ms of network jitter (which the daemon's stamp does see) the control
    // targets 20 ms. Add 0..8 ms of IPC jitter between the stamp and the read and
    // compare arrival = the daemon's stamp with arrival = read time. All six calls
    // run at once with the same jitter pattern, so they share the load.
    let run = |net: u64| {
        (
            std::thread::spawn(move || neteq_delay(ArrivalPolicy::Stamp, net, 0, 12)),
            std::thread::spawn(move || neteq_delay(ArrivalPolicy::Stamp, net, 8_000, 12)),
            std::thread::spawn(move || neteq_delay(ArrivalPolicy::ReadTime, net, 8_000, 12)),
        )
    };
    let runs = [(24_000, run(24_000)), (26_000, run(26_000))];
    eprintln!("NetEq means over the last 5 s (ms), packets 20 ms apart (buffer is noisy: preemptive expand runs the whole call):");
    for (net, (a, b, c)) in runs {
        let (ctl, stamp, read) = (a.join().unwrap(), b.join().unwrap(), c.join().unwrap());
        eprintln!(" network jitter 0..{} ms:", net / 1000);
        for (name, x) in [
            ("control (no IPC jitter)", &ctl),
            ("IPC 0..8, arrival = stamp", &stamp),
            ("IPC 0..8, arrival = read time", &read),
        ] {
            eprintln!(
                "  {name:30} target {:5.1}  preferred {:5.1}  buffer {:5.1}  accel/s {:.2} expand/s {:.2}",
                x.target, x.preferred, x.buffer, x.accel_per_s, x.expand_per_s
            );
        }
        assert!(
            stamp.target <= ctl.target + 1.0,
            "stamp arrival grew the target: {:.1} vs control {:.1}",
            stamp.target,
            ctl.target
        );
        assert!(
            read.target >= stamp.target + 10.0,
            "read-time arrival should grow the target: {:.1} vs {:.1}",
            read.target,
            stamp.target
        );
    }
}

#[test]
fn clock_mismatch_falls_back_to_read_time_and_old_messages_are_dropped() {
    let (mock, conn) = Mock::spawn(key(9), false);
    let cfg = Config {
        seats: vec![(SeatSel::KeyPrefix("0101".into()), 0.0)],
        ..Config::default()
    };
    let c = Client::start(conn, cfg).unwrap();
    let pk = encode_tone(500.0, 0.3, 20, 0, 0);
    let mut seq = 0usize;
    let mut send = |stamp: u64| {
        mock.voice_in(key(1), stamp, env(&pk[seq], 3, seq as u64));
        seq += 1;
        std::thread::sleep(Duration::from_millis(5));
    };
    for _ in 0..3 {
        send(mono_us() + 10_000_000); // the future
    }
    for _ in 0..2 {
        send(1); // over a second old
    }
    for _ in 0..4 {
        send(mono_us() - 150_000); // 150 ms old at read: dropped
    }
    for _ in 0..3 {
        send(mono_us() - 1_000); // fine
    }
    std::thread::sleep(Duration::from_millis(100));
    let stats = c.stop();
    assert_eq!(stats["clock_mismatch"], 5);
    assert_eq!(stats["drops"]["age"], 4);
    let accepted = stats["peers"][key(1).hex()]["packets_accepted"]
        .as_u64()
        .unwrap();
    assert_eq!(
        accepted, 8,
        "mismatched stamps are accepted at read time; only the aged ones are dropped"
    );
    let h = &stats["histograms"]["ipc_delay_us"];
    assert_eq!(h["count"], 3, "only usable stamps feed the IPC histogram");
    assert!(h["p99"].as_u64().unwrap() >= 1_000);
}

#[test]
fn listener_age_is_stamped_per_block_from_the_source_time() {
    let (mock, conn) = Mock::spawn(key(9), false);
    let c = Client::start(conn, Config::default()).unwrap();
    let conn = mock.conn.clone();
    let t = std::thread::spawn(move || {
        let t0 = mono_us();
        for i in 0..180u32 {
            sleep_until(t0 + i as u64 * 11_111);
            let now = mono_us();
            conn.send(&RtMsg::Listener {
                source_mono_us: now,
                forwarded_mono_us: now,
                seq: i,
                pose: Pose {
                    pos: [0.0, 1.6, 0.0],
                    rot: [0.0, 0.0, 0.0, 1.0],
                },
            });
        }
    });
    t.join().unwrap();
    let stats = c.stop();
    let h = &stats["histograms"]["listener_age_us"];
    eprintln!(
        "listener age at 90 Hz source: count {} p50 {} p99 {} p99.9 {} max {} us",
        h["count"], h["p50"], h["p99"], h["p99_9"], h["max"]
    );
    assert!(h["count"].as_u64().unwrap() >= 100);
    // Newest pose is at most one source interval (11.1 ms) plus scheduling old.
    assert!(
        h["p99"].as_u64().unwrap() < 11_111 + 4_000 + 8_000,
        "p99 {}",
        h["p99"]
    );
    assert!(h["p50"].as_u64().unwrap() > 500);
}

#[test]
fn device_change_keeps_incarnation_and_the_packet_clock() {
    let (mock, conn) = Mock::spawn(key(9), false);
    let c = Client::start(
        conn,
        Config {
            capture: CaptureSpec::Tone(440.0),
            incarnation: Some(5150),
            ..Config::default()
        },
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(700));
    c.request_device_change();
    std::thread::sleep(Duration::from_millis(700));
    let stats = c.stop();
    let outs: Vec<_> = mock
        .drain()
        .into_iter()
        .filter_map(|(t, m)| match m {
            RtMsg::VoiceOut { env, .. } => Some((t, env)),
            _ => None,
        })
        .collect();
    assert!(outs.len() > 50);
    assert!(
        outs.iter().all(|(_, e)| e.incarnation == 5150),
        "incarnation kept"
    );
    let mut gaps = 0;
    for w in outs.windows(2) {
        assert_eq!(w[1].1.seq, w[0].1.seq.wrapping_add(1), "seq continuous");
        let d = w[1].1.timestamp.wrapping_sub(w[0].1.timestamp);
        assert!(
            d >= 960 && d % 960 == 0,
            "timestamp advances in whole frames, got {d}"
        );
        if d > 960 {
            gaps += 1;
        }
    }
    // The sample clock still tracks wall time across the change (within a few frames).
    let media_s = outs
        .last()
        .unwrap()
        .1
        .timestamp
        .wrapping_sub(outs[0].1.timestamp) as f64
        / 48000.0;
    let wall_s = (outs.last().unwrap().0 - outs[0].0) as f64 / 1e6;
    eprintln!(
        "device change: media {media_s:.3} s vs wall {wall_s:.3} s, {gaps} packets skipped a gap"
    );
    assert!(
        (media_s - wall_s).abs() < 0.08,
        "media {media_s} wall {wall_s}"
    );
    assert_eq!(stats["device_generation"], 1);
    assert_eq!(stats["sent"]["device_changes"], 1);
}

#[test]
fn playout_latency_from_daemon_read_to_the_output_boundary() {
    // 400 ms of encoded silence, then a tone. The latency is the time between the
    // tone's first packet reaching the client and the first loud output frame at the
    // sink (the mixer runs two blocks ahead of it, so this includes that).
    let buf: Capture = Arc::new(Mutex::new(Vec::new()));
    let (mock, conn) = Mock::spawn(key(9), false);
    let mut cfg = capture_cfg(&buf);
    cfg.seats = vec![(SeatSel::KeyPrefix("0101".into()), 0.0)];
    let c = Client::start(conn, cfg).unwrap();
    let mut packets = encode_tone(500.0, 0.0, 20, 0, 0);
    let tone = encode_tone(500.0, 0.3, 60, 20, 20);
    packets.extend(tone);
    let t0 = mono_us() + 5_000;
    let mut onset_frame = 0usize;
    for (i, p) in packets.iter().enumerate() {
        sleep_until(t0 + i as u64 * 20_000);
        if i == 20 {
            onset_frame = frames(&buf);
        }
        mock.voice_in(key(1), mono_us(), env(p, 3, i as u64));
    }
    std::thread::sleep(Duration::from_millis(200));
    c.stop();
    let out = buf.lock().unwrap().clone();
    let first_loud = out
        .chunks(2)
        .position(|f| f[0].abs().max(f[1].abs()) > 0.05)
        .expect("tone arrived");
    let ms = (first_loud as f64 - onset_frame as f64) / 48.0;
    eprintln!("playout latency, daemon read -> output sink: {ms:.1} ms");
    assert!(ms > 0.0 && ms < 250.0, "{ms}");
}

#[test]
fn revoked_lease_stops_the_client_with_the_reason() {
    let (mock, conn) = Mock::spawn(key(9), false);
    let c = Client::start(conn, Config::default()).unwrap();
    mock.send(&RtMsg::Revoked {
        reason: room_core::rt::revoke::TAKEOVER,
    });
    let t0 = mono_us();
    while c.exit_reason().is_none() && mono_us() - t0 < 1_000_000 {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        c.exit_reason(),
        Some(prim_voice::ExitReason::Revoked(
            room_core::rt::revoke::TAKEOVER
        ))
    );
    c.stop();
}
