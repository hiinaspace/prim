//! The realtime voice lane end to end: leases, the two-daemon voice path, fencing,
//! queue bounds, listener forwarding and delivery delay under core load. Daemons run on
//! the in-memory transport; clients are raw `SOCK_SEQPACKET` sessions built from
//! `room_core::rt`.

#[macro_use]
mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use prim_room::daemon::Start;
use prim_room::local::{b64, unb64, Event, Request, UpdateJson};
use prim_room::rt::{mono_us, SeqSocket};
use room_core::frame::Datagram;
use room_core::ids::Key;
use room_core::rt::{peer_flags, refuse, revoke, Pose, Role, RtMsg, RT_PROTO};
use room_core::voice::{voice_state, VoiceEnvelope, VoiceState, CODEC_OPUS_48K_MONO};

const LM: u8 = voice_state::LISTENING | voice_state::MIC;

fn env(inc: u32, seq: u16, payload: Vec<u8>) -> VoiceEnvelope {
    VoiceEnvelope {
        codec: CODEC_OPUS_48K_MONO,
        incarnation: inc,
        seq,
        timestamp: seq as u32 * 960,
        flags: 0,
        payload,
    }
}

const POSE: Pose = Pose {
    pos: [0.0, 1.6, 0.0],
    rot: [0.0, 0.0, 0.0, 1.0],
};

struct Rt {
    sock: SeqSocket,
    session: u64,
    self_key: Key,
}

enum Hello {
    Welcome(Rt),
    Refused(u8, SeqSocket),
}

async fn hello(path: &Path, role: Role, takeover: bool, inc: u32, flags: u8) -> Hello {
    let sock = SeqSocket::connect(path).unwrap();
    sock.send_msg(&RtMsg::Hello {
        proto: RT_PROTO,
        role,
        takeover,
        incarnation: inc,
        flags,
    })
    .await
    .unwrap();
    match recv(&sock, 2000).await {
        Some(RtMsg::Welcome {
            session, self_key, ..
        }) => Hello::Welcome(Rt {
            sock,
            session,
            self_key,
        }),
        Some(RtMsg::Refused { reason }) => Hello::Refused(reason, sock),
        other => panic!("hello answered with {other:?}"),
    }
}

async fn acquire(path: &Path, role: Role, inc: u32, flags: u8) -> Rt {
    match hello(path, role, false, inc, flags).await {
        Hello::Welcome(rt) => rt,
        Hello::Refused(r, _) => panic!("refused: {r}"),
    }
}

/// Next message within `ms`; `None` on timeout or EOF.
async fn recv(sock: &SeqSocket, ms: u64) -> Option<RtMsg> {
    match tokio::time::timeout(Duration::from_millis(ms), sock.recv_msg()).await {
        Ok(Ok(m)) => m,
        _ => None,
    }
}

/// True if the next thing the socket does within `ms` is to report EOF.
async fn closes(sock: &SeqSocket, ms: u64) -> bool {
    matches!(
        tokio::time::timeout(Duration::from_millis(ms), sock.recv_raw()).await,
        Ok(Ok(None)) | Ok(Err(_))
    )
}

/// Wait for a message that satisfies `pred`, skipping the others, up to `ms`.
async fn wait_msg(sock: &SeqSocket, ms: u64, mut pred: impl FnMut(&RtMsg) -> bool) -> RtMsg {
    let deadline = Instant::now() + Duration::from_millis(ms);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(left, sock.recv_msg()).await {
            Ok(Ok(Some(m))) if pred(&m) => return m,
            Ok(Ok(Some(_))) => {}
            other => panic!("wanted a message, got {other:?}"),
        }
    }
}

fn voice_out(env: VoiceEnvelope) -> RtMsg {
    RtMsg::VoiceOut {
        sent_mono_us: mono_us(),
        env,
    }
}

/// `observer` watches `writer`'s `voice` key until `pred` holds.
async fn wait_voice_key(
    observer: &mut Node,
    writer: Key,
    secs: u64,
    pred: impl Fn(VoiceState) -> bool,
) -> VoiceState {
    let want = format!("member/{}/voice", writer.hex());
    let mut found = None;
    observer
        .wait_event(secs, |e| {
            let Event::Updates { updates, .. } = e else {
                return false;
            };
            for u in updates {
                if let UpdateJson::Value { path, value, .. } = u {
                    if *path == want {
                        if let Ok(v) = VoiceState::decode(&unb64(value).unwrap()) {
                            if pred(v) {
                                found = Some(v);
                                return true;
                            }
                        }
                    }
                }
            }
            false
        })
        .await;
    found.unwrap()
}

async fn subscribe_voice(node: &mut Node) {
    node.ok(|req| Request::Subscribe {
        req,
        sub: 7,
        pattern: "member/*/voice".into(),
    })
    .await;
}

/// Two daemons, connected, both with the voice lane.
async fn pair(fleet: &Fleet) -> (Node, Node) {
    let mut a = fleet
        .start_rt(
            "a",
            Start::Found {
                meta: b"rt".to_vec(),
            },
        )
        .await;
    let token = a.invite().await;
    let mut b = fleet.start_rt("b", Start::Join(token)).await;
    for n in [&mut a, &mut b] {
        eventually!(15, "pair connected", {
            let s = n.status().await;
            s.members.len() >= 2
                && s.members
                    .iter()
                    .filter(|m| m.key != s.me)
                    .all(|m| m.connected)
        });
    }
    (a, b)
}

fn rt_path(n: &Node) -> &Path {
    n.rt_socket.as_deref().unwrap()
}

// ---- tests ------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leases_acquire_busy_takeover_expire_release() {
    let fleet = Fleet::mem();
    let (mut a, mut b) = pair(&fleet).await;
    subscribe_voice(&mut b).await;
    let akey = a.key();
    let path = rt_path(&a).to_path_buf();

    // Acquire.
    let c1 = acquire(&path, Role::Voice, 11, LM).await;
    assert_eq!(c1.self_key, akey);
    let v = wait_voice_key(&mut b, akey, 10, |v| v.incarnation == 11).await;
    assert_eq!(v.flags, LM);
    let st = a.daemon.rt.as_ref().unwrap().snapshot();
    assert_eq!(st.holders.len(), 1);
    assert_eq!(st.holders[0].role, "voice");
    assert_eq!(st.holders[0].session, c1.session);
    assert_eq!(st.holders[0].incarnation, 11);

    // A second launch is refused and closed.
    match hello(&path, Role::Voice, false, 12, LM).await {
        Hello::Refused(r, sock) => {
            assert_eq!(r, refuse::BUSY);
            assert!(closes(&sock, 1000).await, "closed after Refused");
        }
        Hello::Welcome(_) => panic!("second voice launch must be refused"),
    }

    // The tracking_source role is separate.
    let tr = acquire(&path, Role::TrackingSource, 0, 0).await;
    assert_ne!(tr.session, c1.session);

    // A bad hello is refused.
    match hello(&path, Role::Voice, true, 0, LM).await {
        Hello::Refused(r, _) => assert_eq!(r, refuse::BAD_HELLO),
        Hello::Welcome(_) => panic!("incarnation 0 must be refused"),
    }
    assert!(
        c1.sock.try_send_msg(&RtMsg::Keepalive).unwrap(),
        "the refused attempts left the holder alone"
    );

    // Takeover revokes the old session and closes it.
    let c2 = match hello(&path, Role::Voice, true, 22, voice_state::LISTENING).await {
        Hello::Welcome(rt) => rt,
        Hello::Refused(r, _) => panic!("takeover refused: {r}"),
    };
    assert_ne!(c2.session, c1.session);
    assert_eq!(
        wait_msg(&c1.sock, 1000, |m| matches!(m, RtMsg::Revoked { .. })).await,
        RtMsg::Revoked {
            reason: revoke::TAKEOVER
        }
    );
    assert!(closes(&c1.sock, 1000).await);
    let v = wait_voice_key(&mut b, akey, 10, |v| v.incarnation == 22).await;
    assert_eq!(v.flags, voice_state::LISTENING);

    // SetState changes flags and incarnation, and B sees it.
    c2.sock
        .send_msg(&RtMsg::SetState {
            flags: LM,
            incarnation: 23,
        })
        .await
        .unwrap();
    let v = wait_voice_key(&mut b, akey, 10, |v| v.incarnation == 23).await;
    assert_eq!(v.flags, LM);

    // A client that goes silent loses the lease after the TTL (2 s): Revoked{expired},
    // then close; the key is cleared. (It stays connected but sends nothing.)
    let t0 = Instant::now();
    assert_eq!(
        wait_msg(&c2.sock, 4000, |m| matches!(m, RtMsg::Revoked { .. })).await,
        RtMsg::Revoked {
            reason: revoke::EXPIRED
        }
    );
    let waited = t0.elapsed();
    assert!(
        waited > Duration::from_millis(1000) && waited < Duration::from_millis(3200),
        "expired after {waited:?}"
    );
    assert!(closes(&c2.sock, 1000).await);
    wait_voice_key(&mut b, akey, 10, |v| v.flags == 0 && v.incarnation == 0).await;
    assert!(a.daemon.rt.as_ref().unwrap().snapshot().holders.len() <= 1);

    // Closing the socket releases the lease at once.
    let c3 = acquire(&path, Role::Voice, 31, LM).await;
    wait_voice_key(&mut b, akey, 10, |v| v.incarnation == 31).await;
    drop(c3);
    wait_voice_key(&mut b, akey, 10, |v| v.flags == 0 && v.incarnation == 0).await;
    let c4 = acquire(&path, Role::Voice, 41, LM).await; // free again
    drop(c4);
    drop(tr);
    let _ = (&mut a, &mut b);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn voice_flows_between_two_daemons() {
    let fleet = Fleet::mem();
    let (a, b) = pair(&fleet).await;
    let ca = acquire(rt_path(&a), Role::Voice, 5, LM).await;
    let cb = acquire(rt_path(&b), Role::Voice, 7, LM).await;

    // A's client learns that B is connected, listening, with incarnation 7.
    let bkey = b.key();
    let m = wait_msg(&ca.sock, 10_000, |m| {
        matches!(m, RtMsg::Peer { key, flags, incarnation: 7 }
            if *key == bkey
                && flags & (peer_flags::CONNECTED | peer_flags::LISTENING) == (peer_flags::CONNECTED | peer_flags::LISTENING))
    })
    .await;
    assert!(matches!(m, RtMsg::Peer { .. }));

    let sent = env(5, 1, vec![0xfc, 1, 2, 3, 4]);
    ca.sock.send_msg(&voice_out(sent.clone())).await.unwrap();
    let got = wait_msg(&cb.sock, 5_000, |m| matches!(m, RtMsg::VoiceIn { .. })).await;
    let now = mono_us();
    let RtMsg::VoiceIn {
        from,
        received_at_mono_us,
        env: got,
    } = got
    else {
        unreachable!()
    };
    assert_eq!(from, a.key());
    assert_eq!(got, sent);
    assert!(received_at_mono_us <= now, "stamp is not in the future");
    assert!(
        now - received_at_mono_us < 20_000,
        "stamp is {} us old",
        now - received_at_mono_us
    );
    let sa = a.daemon.rt.as_ref().unwrap().snapshot();
    let sb = b.daemon.rt.as_ref().unwrap().snapshot();
    assert_eq!(sa.sent, 1);
    assert_eq!(sa.datagrams_enqueued, 1);
    assert_eq!(sb.received, 1);
    assert_eq!(sa.out_hop.count, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drops_without_a_lease_with_the_wrong_incarnation_and_to_deaf_peers() {
    let fleet = Fleet::mem();
    let (a, b) = pair(&fleet).await;
    let ca = acquire(rt_path(&a), Role::Voice, 5, LM).await;
    let stats_b = b.daemon.rt.clone().unwrap();

    // B has no voice client: arriving voice is dropped and counted.
    for i in 0..5 {
        ca.sock
            .send_msg(&voice_out(env(5, i, vec![9; 40])))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    eventually!(
        5,
        "voice_no_lease counted",
        stats_b.snapshot().drops.no_lease >= 5
    );
    assert_eq!(stats_b.snapshot().received, 0);

    // A VoiceOut whose incarnation isn't the session's is dropped and counted at A.
    let before = a.daemon.rt.as_ref().unwrap().snapshot();
    ca.sock
        .send_msg(&voice_out(env(6, 1, vec![9; 40])))
        .await
        .unwrap();
    eventually!(
        5,
        "wrong incarnation counted",
        a.daemon
            .rt
            .as_ref()
            .unwrap()
            .snapshot()
            .drops
            .wrong_incarnation
            == 1
    );
    assert_eq!(
        a.daemon.rt.as_ref().unwrap().snapshot().sent,
        before.sent,
        "not sent"
    );
    // ... until SetState makes it the session's incarnation.
    ca.sock
        .send_msg(&RtMsg::SetState {
            flags: LM,
            incarnation: 6,
        })
        .await
        .unwrap();
    ca.sock
        .send_msg(&voice_out(env(6, 2, vec![9; 40])))
        .await
        .unwrap();
    eventually!(
        5,
        "accepted after SetState",
        a.daemon.rt.as_ref().unwrap().snapshot().sent == before.sent + 1
    );

    // A peer that says it isn't listening gets nothing from A.
    let cb = acquire(rt_path(&b), Role::Voice, 7, voice_state::MIC).await;
    let bkey = b.key();
    wait_msg(
        &ca.sock,
        10_000,
        |m| matches!(m, RtMsg::Peer { key, incarnation: 7, .. } if *key == bkey),
    )
    .await;
    let enq = a.daemon.rt.as_ref().unwrap().snapshot().datagrams_enqueued;
    ca.sock
        .send_msg(&voice_out(env(6, 3, vec![9; 40])))
        .await
        .unwrap();
    eventually!(
        5,
        "sent counted",
        a.daemon.rt.as_ref().unwrap().snapshot().sent == before.sent + 2
    );
    assert_eq!(
        a.daemon.rt.as_ref().unwrap().snapshot().datagrams_enqueued,
        enq,
        "B is not listening"
    );
    while let Some(m) = recv(&cb.sock, 200).await {
        assert!(!matches!(m, RtMsg::VoiceIn { .. }), "a deaf peer got voice");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_session_inherits_nothing_of_the_old_one() {
    let fleet = Fleet::mem();
    let (a, b) = pair(&fleet).await;
    let ca = acquire(rt_path(&a), Role::Voice, 5, LM).await;
    // B's holder never reads, so what A sends piles up there.
    let cb1 = acquire(rt_path(&b), Role::Voice, 7, LM).await;
    let bkey = b.key();
    wait_msg(
        &ca.sock,
        10_000,
        |m| matches!(m, RtMsg::Peer { key, incarnation: 7, .. } if *key == bkey),
    )
    .await;
    let stats_b = b.daemon.rt.clone().unwrap();
    // Fill the holder's socket buffer so messages stay in the daemon's queue.
    for i in 0..300u16 {
        ca.sock
            .send_msg(&voice_out(env(5, i, vec![0xAA; 900])))
            .await
            .unwrap();
        if i % 10 == 9 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    eventually!(5, "B queued something", stats_b.snapshot().queue_depth > 0);

    // The voice client restarts: a new session on B (takeover, new incarnation).
    let cb2 = match hello(rt_path(&b), Role::Voice, true, 8, LM).await {
        Hello::Welcome(rt) => rt,
        Hello::Refused(r, _) => panic!("{r}"),
    };
    assert_eq!(stats_b.snapshot().queue_depth, 0, "old queue discarded");
    // Nothing of the old session (or of A's earlier packets) reaches the new one.
    let mut stale = 0;
    while let Some(m) = recv(&cb2.sock, 300).await {
        if matches!(m, RtMsg::VoiceIn { .. }) {
            stale += 1;
        }
    }
    assert_eq!(stale, 0, "no VoiceIn from before the restart");
    drop(cb1);

    // A new packet does arrive.
    ca.sock
        .send_msg(&voice_out(env(5, 1000, vec![0xBB; 50])))
        .await
        .unwrap();
    let m = wait_msg(&cb2.sock, 5_000, |m| matches!(m, RtMsg::VoiceIn { .. })).await;
    assert!(matches!(m, RtMsg::VoiceIn { env, .. } if env.seq == 1000));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inbound_queue_is_bounded_in_size_and_age() {
    let fleet = Fleet::mem();
    let (a, _b) = pair(&fleet).await;
    let shared = a.daemon.rt.clone().unwrap();
    let holder = acquire(rt_path(&a), Role::Voice, 5, LM).await; // never reads yet

    // Many senders (fake members) over the total cap, then one over its own cap.
    for i in 0..20u64 {
        shared.fake_member(1000 + i, Key([i as u8 + 1; 32]));
    }
    let offer = |conn: u64, seq: u16| {
        let bytes = Datagram::Voice(env(9, seq, vec![7; 900])).encode();
        assert!(shared.offer(conn, bytes).is_none());
    };
    // Per sender: 100 from one sender.
    for s in 0..100u16 {
        offer(1000, s);
    }
    eventually!(5, "lane drained ingress", {
        let d = shared.snapshot();
        d.drops.per_sender > 0 || d.queue_depth > 0
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let d = shared.snapshot();
    assert!(
        d.queue_depth <= 32,
        "depth {} over the per-sender cap",
        d.queue_depth
    );
    assert!(d.drops.per_sender > 0, "{d:?}");

    // Total: 20 senders x 30 each = 600 more.
    for round in 0..30u16 {
        for i in 0..20u64 {
            offer(1000 + i, 200 + round);
        }
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let d = shared.snapshot();
    assert!(d.queue_depth <= 256, "depth {} over 256", d.queue_depth);
    assert!(d.drops.queue_full > 0 || d.drops.per_sender > 0, "{d:?}");
    // Not a member at all.
    assert!(shared
        .offer(9999, Datagram::Voice(env(9, 1, vec![1])).encode())
        .is_none());
    eventually!(
        5,
        "not_member counted",
        shared.snapshot().drops.not_member == 1
    );
    // Garbage is counted as a decode failure.
    assert!(shared.offer(1000, vec![0x12, 0xff, 0xff]).is_none());
    eventually!(5, "decode counted", shared.snapshot().drops.decode == 1);

    // Age: the holder was stalled well past 100 ms. Read it all now.
    tokio::time::sleep(Duration::from_millis(250)).await;
    let mut delivered = 0u64;
    while let Some(m) = recv(&holder.sock, 300).await {
        if matches!(m, RtMsg::VoiceIn { .. }) {
            delivered += 1;
        }
    }
    let d = shared.snapshot();
    println!(
        "stalled holder: delivered={delivered} drops={:?} in_queue max={} us",
        d.drops, d.in_queue.max_us
    );
    assert!(
        d.drops.age > 0,
        "aged-out messages are dropped, not delivered late"
    );
    assert!(
        d.in_queue.max_us <= 105_000,
        "a delivery spent {} us in the daemon after the stall",
        d.in_queue.max_us
    );
    assert_eq!(d.queue_depth, 0);

    // Fresh traffic after the stall is delivered young.
    offer(1001, 500);
    let m = wait_msg(&holder.sock, 2_000, |m| matches!(m, RtMsg::VoiceIn { .. })).await;
    let RtMsg::VoiceIn {
        received_at_mono_us,
        ..
    } = m
    else {
        unreachable!()
    };
    assert!(mono_us() - received_at_mono_us < 100_000);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn listener_poses_are_forwarded_at_once_and_replaced_not_queued() {
    let fleet = Fleet::mem();
    let (a, _b) = pair(&fleet).await;
    let shared = a.daemon.rt.clone().unwrap();
    let voice = acquire(rt_path(&a), Role::Voice, 5, LM).await;
    let track = acquire(rt_path(&a), Role::TrackingSource, 0, 0).await;

    // 90 Hz: each pose reaches the voice client within a few ms.
    let mut lat = Vec::new();
    for seq in 0..90u32 {
        let src = mono_us();
        track
            .sock
            .send_msg(&RtMsg::PoseOut {
                source_mono_us: src,
                seq,
                pose: POSE,
            })
            .await
            .unwrap();
        let m = wait_msg(&voice.sock, 1_000, |m| matches!(m, RtMsg::Listener { .. })).await;
        let RtMsg::Listener {
            source_mono_us,
            forwarded_mono_us,
            seq: s,
            pose,
        } = m
        else {
            unreachable!()
        };
        assert_eq!((source_mono_us, s, pose), (src, seq, POSE));
        assert!(forwarded_mono_us >= src);
        lat.push(mono_us() - src);
        tokio::time::sleep(Duration::from_micros(11_111)).await;
    }
    lat.sort_unstable();
    let p99 = lat[lat.len() * 99 / 100];
    println!(
        "pose source -> voice client read: p50={} us p99={} us",
        lat[lat.len() / 2],
        p99
    );
    assert!(p99 < 5_000, "p99 {p99} us");
    let h = shared.snapshot().pose_hop;
    assert_eq!(h.count, 90);
    assert!(h.p99_us < 5_000);

    // A stalled reader gets the newest pose, never a backlog.
    let n = 3000u32;
    for seq in 1000..1000 + n {
        track
            .sock
            .send_msg(&RtMsg::PoseOut {
                source_mono_us: mono_us(),
                seq,
                pose: POSE,
            })
            .await
            .unwrap();
    }
    eventually!(
        5,
        "poses replaced",
        shared.snapshot().drops.pose_replaced > 0
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut last = None;
    let mut count = 0u32;
    while let Some(m) = recv(&voice.sock, 200).await {
        if let RtMsg::Listener { seq, .. } = m {
            count += 1;
            last = Some(seq);
        }
    }
    println!(
        "stalled reader got {count} of {n} poses, replaced={}",
        shared.snapshot().drops.pose_replaced
    );
    assert_eq!(last, Some(1000 + n - 1), "the newest pose arrives last");
    assert!(count < n, "poses were replaced, not queued");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rt_delay_stays_small_while_the_core_is_flooded() {
    let fleet = Fleet::mem();
    let (a, b) = pair(&fleet).await;
    let ca = acquire(rt_path(&a), Role::Voice, 5, LM).await;
    let cb = acquire(rt_path(&b), Role::Voice, 7, LM).await;
    let bkey = b.key();
    wait_msg(&ca.sock, 10_000, |m| {
        matches!(m, RtMsg::Peer { key, incarnation: 7, flags } if *key == bkey && flags & peer_flags::CONNECTED != 0)
    })
    .await;

    // Flood A's core: state writes and reliable messages to B over its local socket.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flood = {
        let socket = a.socket.clone();
        let stop = stop.clone();
        let to = bkey.hex();
        tokio::spawn(async move {
            let mut c = prim_room::local::Client::connect(&socket).await.unwrap();
            let mut i = 0u64;
            let mut sent = 0u64;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                for _ in 0..10 {
                    let req = c.next_req();
                    c.send(&Request::Write {
                        req,
                        scope: "member".into(),
                        name: format!("flood/k{}", i % 400),
                        live: false,
                        persistent: false,
                        value: b64(&[(i % 251) as u8; 150]),
                    })
                    .await
                    .unwrap();
                    i += 1;
                    let req = c.next_req();
                    c.send(&Request::Send {
                        req,
                        to: to.clone(),
                        channel: 3,
                        reliable: true,
                        payload: b64(&vec![1u8; 8_000]),
                    })
                    .await
                    .unwrap();
                    sent += 1;
                }
                // Drain replies so the daemon never blocks on us.
                // Drain replies, which also paces the flood below the daemon's per-connection
                // queue cap (16 MiB) that would close the connection.
                while let Ok(Ok(Some(_))) =
                    tokio::time::timeout(Duration::from_millis(1), c.next_event()).await
                {
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            (i, sent)
        })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Voice at 2 ms spacing (denser than the real 20 ms, for samples). The envelope's
    // timestamp field carries the send time (low 32 bits of us), so the receiver can
    // compute end-to-end delay on the shared monotonic clock.
    let n = 1500u16;
    let delays = Arc::new(Mutex::new(Vec::<u64>::new()));
    let cb_sock = Arc::new(cb.sock);
    let keepalive = {
        let sock = cb_sock.clone();
        tokio::spawn(async move {
            loop {
                let _ = sock.try_send_msg(&RtMsg::Keepalive);
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        })
    };
    let reader = {
        let delays = delays.clone();
        let cb_sock = cb_sock.clone();
        tokio::spawn(async move {
            let mut got = 0u32;
            loop {
                match tokio::time::timeout(Duration::from_millis(1500), cb_sock.recv_msg()).await {
                    Ok(Ok(Some(RtMsg::VoiceIn { env, .. }))) => {
                        let now = mono_us() as u32;
                        delays
                            .lock()
                            .unwrap()
                            .push(now.wrapping_sub(env.timestamp) as u64);
                        got += 1;
                    }
                    Ok(Ok(Some(_))) => {}
                    _ => break,
                }
            }
            got
        })
    };
    for seq in 0..n {
        let mut e = env(5, seq, vec![0x55; 80]);
        e.timestamp = mono_us() as u32;
        ca.sock.send_msg(&voice_out(e)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let (writes, msgs) = flood.await.unwrap();
    let got = reader.await.unwrap();
    keepalive.abort();
    let mut d = delays.lock().unwrap().clone();
    d.sort_unstable();
    let pct = |p: f64| d[((p * d.len() as f64).ceil() as usize).clamp(1, d.len()) - 1];
    println!(
        "core flood: {writes} state writes + {msgs} x 8 KB reliable messages while {got}/{n} voice packets crossed A -> B"
    );
    println!(
        "end-to-end (client A write -> client B read) us: p50={} p99={} p99.9={} max={}",
        pct(0.5),
        pct(0.99),
        pct(0.999),
        d[d.len() - 1]
    );
    let sa = a.daemon.rt.as_ref().unwrap().snapshot();
    let sb = b.daemon.rt.as_ref().unwrap().snapshot();
    println!(
        "A out_hop (client write -> lane read) us: p50={} p99={} p99.9={} | B in_queue (stamp -> write) us: p50={} p99={} p99.9={}",
        sa.out_hop.p50_us, sa.out_hop.p99_us, sa.out_hop.p999_us,
        sb.in_queue.p50_us, sb.in_queue.p99_us, sb.in_queue.p999_us
    );
    {
        let mut a = a;
        let st = a.status().await;
        for r in &st.routes {
            println!("A route: {r:?}");
        }
        println!(
            "A rt: sent={} enq={} drops={:?}",
            sa.sent, sa.datagrams_enqueued, sa.drops
        );
        println!(
            "B rt: received={} drops={:?} depth={}",
            sb.received, sb.drops, sb.queue_depth
        );
    }
    assert!(writes > 100 && msgs > 100, "the flood actually ran");
    assert!(got as usize >= n as usize * 9 / 10, "{got} of {n} arrived");
    assert!(pct(0.99) < 5_000, "p99 {} us", pct(0.99));
}
