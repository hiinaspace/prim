//! The rt reader: decodes daemon messages, fences voice by incarnation per sender and
//! feeds each sender's playout receiver with the daemon's arrival stamp.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use room_core::ids::Key;
use room_core::rt::{peer_flags, Role, RtMsg};
use room_core::voice::{voice_flags, Fence, VoiceEnvelope};
use voice_core::{
    PacketFlags, PacketObservation, PlayoutConfig, PlayoutReceiver, PushOutcome, VoicePacket,
};

use crate::clock::mono_us;
use crate::config::ArrivalPolicy;
use crate::shared::{ExitReason, InEntry, Inbox, ListenerState, PeerRx, Shared, INBOX_MAX};
use crate::tone;

/// A `VoiceIn` older than this at read is dropped (VOICE.md, queue bounds).
pub const MAX_AGE_US: u64 = 100_000;
/// Before any packet of the session has been judged matched, a stamp more than this old
/// means the clock domains don't match.
pub const MISMATCH_AGE_US: u64 = 1_000_000;
pub const KEEPALIVE_US: u64 = 250_000;

pub fn spawn(sh: Arc<Shared>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("voice-rt-reader".into())
        .spawn(move || reader_loop(&sh))
        .expect("spawn reader")
}

fn reader_loop(sh: &Shared) {
    // Per rt session: set by the first packet judged matched, never cleared within a session.
    // A reconnect is a new session, so the latch starts unset again.
    let mut matched = false;
    while !sh.is_shutdown() {
        if !sh.link.is_up() {
            if !reconnect(sh) {
                return;
            }
            matched = false;
        }
        let conn = sh.link.get();
        let got = conn.recv(Duration::from_millis(20));
        match got {
            Ok(Some(b)) => {
                let read_us = mono_us();
                match RtMsg::decode(&b) {
                    Ok(m) => {
                        if let Flow::Reconnect = handle(sh, m, read_us, &mut matched) {
                            sh.link.set_up(false);
                            continue;
                        }
                    }
                    Err(_) => sh.stats.bump("drop_rt_decode"),
                }
            }
            Ok(None) => {}
            Err(_) => {
                if sh.cfg.rt_socket.is_some() {
                    sh.link.set_up(false);
                    continue;
                }
                sh.stop(ExitReason::SocketClosed);
                return;
            }
        }
        if sh.is_shutdown() {
            return;
        }
        // Keepalive when idle: any message renews the lease, so only send if we haven't.
        let now = mono_us();
        if now.saturating_sub(sh.last_send_us.load(Ordering::Relaxed)) >= KEEPALIVE_US {
            match conn.send(&RtMsg::Keepalive) {
                crate::rtio::SendOutcome::Closed => {
                    if sh.cfg.rt_socket.is_some() {
                        sh.link.set_up(false);
                        continue;
                    }
                    sh.stop(ExitReason::SocketClosed);
                    return;
                }
                _ => {}
            }
            sh.last_send_us.store(now, Ordering::Relaxed);
            sh.stats.bump("keepalives");
        }
    }
}

/// Reconnect backoff: 50 ms, doubling to 1 s.
pub fn backoff_ms(attempt: u32) -> u64 {
    (50u64 << attempt.min(5)).min(1000)
}

/// How long a reconnect keeps trying while the daemon says another client holds the role.
pub const BUSY_RETRY_US: u64 = 3_000_000;

/// Connect again with the same incarnation and the current state, and swap the new socket
/// in. `false` when the client is stopping (fatal revoke, busy for too long, shutdown).
fn reconnect(sh: &Shared) -> bool {
    let Some(path) = sh.cfg.rt_socket.clone() else {
        sh.stop(ExitReason::SocketClosed);
        return false;
    };
    sh.stats.bump("reconnects");
    let t0 = mono_us();
    let mut busy_since: Option<u64> = None;
    let mut attempt = 0u32;
    loop {
        if sh.is_shutdown() {
            return false;
        }
        match crate::rtio::RtConn::connect(&path) {
            Ok(c) => {
                let flags = sh.ctl.wire_flags(&sh.cfg);
                match crate::rtio::handshake(&c, Role::Voice, false, sh.incarnation, flags) {
                    Ok(lease) => {
                        if lease.self_key != sh.self_key {
                            sh.stop(ExitReason::SocketClosed);
                            return false;
                        }
                        sh.link.replace(c);
                        sh.last_send_us.store(mono_us(), Ordering::Relaxed);
                        sh.stats.add("reconnect_down_us", mono_us() - t0);
                        sh.send_cmd(crate::control::Cmd::Announce);
                        return true;
                    }
                    Err(crate::rtio::HandshakeError::Refused(r)) => {
                        if r != room_core::rt::refuse::BUSY {
                            sh.stop(ExitReason::Refused(r));
                            return false;
                        }
                        let since = *busy_since.get_or_insert_with(mono_us);
                        if mono_us() - since > BUSY_RETRY_US {
                            sh.stop(ExitReason::Refused(r));
                            return false;
                        }
                    }
                    Err(crate::rtio::HandshakeError::Revoked(r)) => {
                        if !reconnectable_revoke(r) {
                            sh.stop(ExitReason::Revoked(r));
                            return false;
                        }
                    }
                    Err(_) => {}
                }
            }
            Err(_) => {}
        }
        sh.stats.bump("reconnect_attempts");
        let wait = backoff_ms(attempt);
        attempt += 1;
        let until = mono_us() + wait * 1000;
        while mono_us() < until && !sh.is_shutdown() {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// An expired lease is retried; a takeover, a policy revoke or a code we do not know is final.
pub fn reconnectable_revoke(reason: u8) -> bool {
    reason == room_core::rt::revoke::EXPIRED
}

enum Flow {
    Continue,
    Reconnect,
}

fn handle(sh: &Shared, m: RtMsg, read_us: u64, matched: &mut bool) -> Flow {
    match m {
        RtMsg::VoiceIn {
            from,
            received_at_mono_us,
            env,
        } => voice_in(sh, from, received_at_mono_us, env, read_us, matched),
        RtMsg::Listener {
            source_mono_us,
            forwarded_mono_us,
            seq,
            pose,
        } => {
            *sh.listener.lock().unwrap() = Some(ListenerState {
                source_mono_us,
                forwarded_mono_us,
                seq,
                pose,
            });
        }
        RtMsg::Peer { key, flags, .. } => {
            if key != sh.self_key && flags & peer_flags::CONNECTED != 0 {
                sh.seats.lock().unwrap().add(key);
            }
        }
        RtMsg::VoiceControl {
            req,
            mic,
            deaf,
            tray_live,
        } => sh.send_cmd(crate::control::Cmd::Apply {
            req,
            mic,
            deaf,
            tray_live,
        }),
        RtMsg::TrackingLease { held } => tracking_lease(sh, held),
        RtMsg::Revoked { reason } => {
            if sh.cfg.rt_socket.is_some() && reconnectable_revoke(reason) {
                sh.stats.bump("revoked_expired");
                return Flow::Reconnect;
            }
            sh.stop(ExitReason::Revoked(reason));
        }
        _ => {}
    }
    Flow::Continue
}

/// The tracking lease changed hands. When it is released the head pose is gone: the listener
/// falls back to the seat's origin pose. And unless the tray is the mute authority
/// (`tray_live`), the microphone must not stay open with nobody watching it: mute.
fn tracking_lease(sh: &Shared, held: bool) {
    let was = sh.ctl.tracking_held.swap(held, Ordering::SeqCst);
    if held || !was {
        return;
    }
    *sh.listener.lock().unwrap() = None;
    sh.stats.bump("listener_cleared");
    // Whether to mute is decided on the control thread, after every command queued before
    // this one: an unmute still in the queue must not slip past the safety mute.
    sh.send_cmd(crate::control::Cmd::TrackingLost);
}

/// What to do with one `VoiceIn`, decided from its stamps alone.
#[derive(Debug, PartialEq, Eq)]
pub enum Timing {
    /// Use this arrival time; `delay_us` is the IPC delay to record (None if the
    /// stamp was unusable and the read time stands in for it).
    Accept {
        arrival_us: u64,
        delay_us: Option<u64>,
    },
    /// Over 100 ms old at read.
    TooOld,
}

/// `matched` is the session's latch: once any packet has a stamp that is not in the
/// future and at most 1 s old, the clock domains match for the rest of the session, and
/// a stamp over 100 ms old is `TooOld`, never a mismatch. Until then a stamp in the
/// future or over 1 s old is a mismatch and the read time stands in.
pub fn judge_timing(
    received_at_us: u64,
    read_us: u64,
    policy: ArrivalPolicy,
    matched: &mut bool,
    mismatch: &mut bool,
) -> Timing {
    *mismatch = false;
    let in_future = received_at_us > read_us;
    if !in_future && read_us - received_at_us <= MISMATCH_AGE_US {
        *matched = true;
    }
    if in_future || read_us - received_at_us > MISMATCH_AGE_US {
        if !*matched {
            *mismatch = true;
            return Timing::Accept {
                arrival_us: read_us,
                delay_us: None,
            };
        }
        if in_future {
            // Same clock, so this can't happen; treat it as no delay.
            return Timing::Accept {
                arrival_us: read_us,
                delay_us: None,
            };
        }
        return Timing::TooOld;
    }
    let delay = read_us - received_at_us;
    if delay > MAX_AGE_US {
        return Timing::TooOld;
    }
    Timing::Accept {
        arrival_us: match policy {
            ArrivalPolicy::Stamp => received_at_us,
            ArrivalPolicy::ReadTime => read_us,
        },
        delay_us: Some(delay),
    }
}

fn new_receiver(sh: &Shared) -> voice_core::Result<PlayoutReceiver> {
    match sh.cfg.min_delay_ms {
        None => PlayoutReceiver::new(),
        Some(min) => PlayoutReceiver::with_config(PlayoutConfig {
            min_delay_ms: min,
            max_delay_ms: 250.max(min),
            ..PlayoutConfig::default()
        }),
    }
}

fn voice_in(
    sh: &Shared,
    from: Key,
    received_at_us: u64,
    env: VoiceEnvelope,
    read_us: u64,
    matched: &mut bool,
) {
    sh.stats.bump("voice_in");
    let mut mismatch = false;
    let (arrival_us, delay_us) = match judge_timing(
        received_at_us,
        read_us,
        sh.cfg.arrival,
        matched,
        &mut mismatch,
    ) {
        Timing::Accept {
            arrival_us,
            delay_us,
        } => (arrival_us, delay_us),
        Timing::TooOld => {
            sh.stats.bump("drop_age");
            return;
        }
    };
    if mismatch {
        sh.stats.bump("clock_mismatch");
    }
    if let Some(d) = delay_us {
        sh.stats.inner.lock().unwrap().ipc_delay_us.record(d);
    }
    sh.seats.lock().unwrap().add(from);

    let inbox = {
        let mut senders = sh.senders.lock().unwrap();
        let inbox = senders.inboxes.entry(from).or_default().clone();
        senders
            .map
            .entry(from)
            .or_insert_with(|| Arc::new(Mutex::new(PeerRx::with_inbox(inbox.clone()))));
        inbox
    };
    let pkt = VoicePacket {
        seq: env.seq,
        timestamp: env.timestamp,
        // The wire's low three flag bits are voice-core's; STREAM_START is ours alone.
        flags: PacketFlags::from_bits(
            env.flags & (voice_flags::KNOWN & !voice_flags::STREAM_START),
        ),
        payload: env.payload,
    };
    // Both observations go in: the delay estimator uses `arrival_us` (the daemon's stamp,
    // or the read time under the ReadTime experiment); `read_us` is when this process had
    // the packet, and lets the receiver tell local lateness from network lateness.
    let obs = PacketObservation {
        daemon_arrival_us: arrival_us,
        local_us: read_us,
    };
    let t_wait = std::time::Instant::now();
    let outcome = enqueue(&inbox, env.incarnation, pkt, obs);
    sh.stats
        .inner
        .lock()
        .unwrap()
        .reader_peer_wait_us
        .record(t_wait.elapsed().as_micros() as u64);
    match outcome {
        Enqueue::Stale => sh.stats.bump("fence_stale"),
        Enqueue::NewStream => {
            sh.stats.bump("fence_new_stream");
            let hz = tone::auto_hz(env.incarnation);
            let mut tones = sh.tones.lock().unwrap();
            if !tones.iter().any(|(i, _)| *i == env.incarnation) {
                tones.push((env.incarnation, hz));
            }
        }
        Enqueue::Queued => {}
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Enqueue {
    /// An older incarnation than the current one: dropped.
    Stale,
    /// First packet of a new incarnation: a reset is queued ahead of it.
    NewStream,
    Queued,
}

/// The reader's half of the hand-off: fence by incarnation and queue the packet for the mixer.
/// Takes only the inbox's own lock (a `Vec::push`), never the sender's `PeerRx` lock. A new
/// incarnation clears whatever the old one left queued and puts a `Reset` first, so queued
/// packets of the old stream can never be pushed into the new one.
pub fn enqueue(inbox: &Inbox, incarnation: u32, pkt: VoicePacket, obs: PacketObservation) -> Enqueue {
    let mut st = inbox.state.lock().unwrap();
    let mut out = Enqueue::Queued;
    match st.fence.judge(incarnation) {
        Fence::Stale => {
            st.stale += 1;
            return Enqueue::Stale;
        }
        Fence::NewStream { .. } => {
            st.entries.clear();
            st.entries.push(InEntry::Reset { incarnation });
            out = Enqueue::NewStream;
        }
        Fence::Current => {}
    }
    if st.entries.len() >= INBOX_MAX {
        // Keep the Reset (if any, it is first) and drop the oldest packet.
        if let Some(i) = st.entries.iter().position(|e| matches!(e, InEntry::Packet { .. })) {
            st.entries.remove(i);
            st.overflow += 1;
        }
    }
    st.entries.push(InEntry::Packet { pkt, obs });
    out
}

/// The mixer's half: apply what the reader queued, in order, to the sender's receiver. Called
/// with the sender's `PeerRx` lock held, at the start of each block.
pub fn drain_inbox(sh: &Shared, p: &mut PeerRx) {
    {
        let mut st = p.inbox.state.lock().unwrap();
        if st.entries.is_empty() {
            return;
        }
        std::mem::swap(&mut st.entries, &mut p.scratch);
    }
    let mut entries = std::mem::take(&mut p.scratch);
    for e in entries.drain(..) {
        match e {
            InEntry::Reset { incarnation } => {
                p.new_streams += 1;
                p.incarnation = incarnation;
                p.needs_reset = true;
                match p.receiver.as_mut() {
                    Some(r) => {
                        let _ = r.reset_stream();
                    }
                    None => match new_receiver(sh) {
                        Ok(r) => p.receiver = Some(r),
                        Err(_) => {
                            p.push_errors += 1;
                            sh.stats.bump("drop_push_error");
                        }
                    },
                }
            }
            InEntry::Packet { pkt, obs } => {
                let Some(rx) = p.receiver.as_mut() else {
                    continue;
                };
                match rx.push_packet(pkt, obs) {
                    PushOutcome::Rejected => {
                        p.push_errors += 1;
                        sh.stats.bump("drop_push_error");
                    }
                    _ => {
                        p.packets_accepted += 1;
                        p.last_packet_us = obs.local_us;
                        p.active = true;
                    }
                }
            }
        }
    }
    p.scratch = entries;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_pkt(seq: u16) -> VoicePacket {
        VoicePacket {
            seq,
            timestamp: seq as u32 * 960,
            flags: PacketFlags::from_bits(0),
            payload: vec![0u8; 4],
        }
    }
    fn test_obs() -> PacketObservation {
        PacketObservation { daemon_arrival_us: 0, local_us: 0 }
    }

    #[test]
    fn enqueue_never_waits_on_peer_lock() {
        // The mixer holds the sender's PeerRx lock across decode; the reader must not wait.
        let inbox = Arc::new(Inbox::default());
        let peer = Arc::new(Mutex::new(PeerRx::with_inbox(inbox.clone())));
        let held = peer.lock().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let ib = inbox.clone();
        std::thread::spawn(move || {
            for i in 0..50 {
                enqueue(&ib, 1, test_pkt(i), test_obs());
            }
            tx.send(()).unwrap();
        });
        rx.recv_timeout(Duration::from_secs(2))
            .expect("reader blocked while the peer lock was held");
        drop(held);
        assert_eq!(inbox.state.lock().unwrap().entries.len(), 51); // Reset + 50
    }

    #[test]
    fn new_incarnation_clears_old_queue_and_resets_first() {
        let inbox = Inbox::default();
        assert_eq!(enqueue(&inbox, 5, test_pkt(1), test_obs()), Enqueue::NewStream);
        enqueue(&inbox, 5, test_pkt(2), test_obs());
        assert_eq!(enqueue(&inbox, 6, test_pkt(1), test_obs()), Enqueue::NewStream);
        assert_eq!(enqueue(&inbox, 5, test_pkt(3), test_obs()), Enqueue::Stale);
        let st = inbox.state.lock().unwrap();
        assert!(matches!(st.entries[0], InEntry::Reset { incarnation: 6 }));
        assert_eq!(st.entries.len(), 2);
        assert_eq!(st.stale, 1);
    }

    #[test]
    fn inbox_is_bounded() {
        let inbox = Inbox::default();
        for i in 0..(INBOX_MAX as u16 + 40) {
            enqueue(&inbox, 1, test_pkt(i), test_obs());
        }
        let st = inbox.state.lock().unwrap();
        assert!(st.entries.len() <= INBOX_MAX);
        assert!(matches!(st.entries[0], InEntry::Reset { .. }));
        assert!(st.overflow > 0);
    }

    #[test]
    fn timing_rules() {
        let mut mm = false;
        let mut matched = false;
        let t = judge_timing(
            1_000_000,
            1_003_000,
            ArrivalPolicy::Stamp,
            &mut matched,
            &mut mm,
        );
        assert_eq!(
            t,
            Timing::Accept {
                arrival_us: 1_000_000,
                delay_us: Some(3_000)
            }
        );
        assert!(!mm);
        let t = judge_timing(
            1_000_000,
            1_003_000,
            ArrivalPolicy::ReadTime,
            &mut matched,
            &mut mm,
        );
        assert_eq!(
            t,
            Timing::Accept {
                arrival_us: 1_003_000,
                delay_us: Some(3_000)
            }
        );
        // over 100 ms old: dropped
        assert_eq!(
            judge_timing(
                1_000_000,
                1_150_000,
                ArrivalPolicy::Stamp,
                &mut matched,
                &mut mm
            ),
            Timing::TooOld
        );
        // future stamp, and over 1 s old, before any packet matched: read time stands
        // in, counted
        matched = false;
        let t = judge_timing(
            9_000_000,
            1_000_000,
            ArrivalPolicy::Stamp,
            &mut matched,
            &mut mm,
        );
        assert_eq!(
            t,
            Timing::Accept {
                arrival_us: 1_000_000,
                delay_us: None
            }
        );
        assert!(mm);
        let t = judge_timing(10, 2_000_000, ArrivalPolicy::Stamp, &mut matched, &mut mm);
        assert_eq!(
            t,
            Timing::Accept {
                arrival_us: 2_000_000,
                delay_us: None
            }
        );
        assert!(mm);
    }

    #[test]
    fn stall_replay_older_than_a_second_is_dropped_once_matched() {
        let mut mm = false;
        let mut matched = false;
        // Live packets first: the session is matched.
        let t = judge_timing(
            1_000_000,
            1_002_000,
            ArrivalPolicy::Stamp,
            &mut matched,
            &mut mm,
        );
        assert!(matches!(t, Timing::Accept { .. }));
        assert!(matched && !mm);
        // The client stalled 1.5 s; the socket backlog is read on resume, stamps 1.0-1.5 s old.
        let resume = 2_500_000;
        for age in [1_500_000u64, 1_200_000, 1_000_001, 400_000, 101_000] {
            let t = judge_timing(
                resume - age,
                resume,
                ArrivalPolicy::Stamp,
                &mut matched,
                &mut mm,
            );
            assert_eq!(t, Timing::TooOld, "age {age}");
            assert!(!mm, "never a mismatch once matched (age {age})");
        }
        let t = judge_timing(
            resume - 5_000,
            resume,
            ArrivalPolicy::Stamp,
            &mut matched,
            &mut mm,
        );
        assert!(matches!(
            t,
            Timing::Accept {
                delay_us: Some(5_000),
                ..
            }
        ));
    }

    #[test]
    fn plus_minus_five_second_offset_is_still_a_mismatch_and_accepted() {
        for (stamp, read) in [(9_000_000u64, 4_000_000u64), (4_000_000, 9_000_000)] {
            let mut mm = false;
            let mut matched = false;
            for _ in 0..3 {
                let t = judge_timing(stamp, read, ArrivalPolicy::Stamp, &mut matched, &mut mm);
                assert_eq!(
                    t,
                    Timing::Accept {
                        arrival_us: read,
                        delay_us: None
                    }
                );
                assert!(mm && !matched);
            }
        }
    }

    #[test]
    fn matched_latch_ignores_a_single_old_stamp_before_it_is_set() {
        // First packet already 500 ms old: matched (<= 1 s), but TooOld.
        let mut mm = false;
        let mut matched = false;
        let t = judge_timing(
            1_000_000,
            1_500_000,
            ArrivalPolicy::Stamp,
            &mut matched,
            &mut mm,
        );
        assert_eq!(t, Timing::TooOld);
        assert!(matched && !mm);
    }
}
