//! The OLD (pre-timeline) NetEq VoiceReceiver, fed the same events file listen_render writes.
//! Build it inside a scratch worktree of the submodule at 56b4c31 (see tools/listen/README):
//!   cp this file to voice-core/examples/ ; cargo run --release --example legacy_render -- IN.events OUT.f32 STATS.json
//! The old receiver reads Instant::now internally, so this runs in REAL time (about the clip
//! length plus 2.5 s). It never touches an audio device.
use std::time::{Duration, Instant};
use voice_core::{PacketArrival, PacketFlags, VoicePacket, VoiceReceiver};

const START: u64 = 1_000_000;
const TAIL_US: u64 = 1_500_000;

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mut ev: Vec<(u64, u64, VoicePacket)> = Vec::new();
    for line in std::fs::read_to_string(&a[0]).unwrap().lines() {
        let f: Vec<&str> = line.split(' ').collect();
        ev.push((
            f[0].parse().unwrap(),
            f[1].parse().unwrap(),
            VoicePacket {
                seq: f[2].parse().unwrap(),
                timestamp: f[3].parse().unwrap(),
                flags: PacketFlags::from_bits(f[4].parse().unwrap()),
                payload: unhex(f.get(5).copied().unwrap_or("")),
            },
        ));
    }
    let end_us = ev.last().map(|e| e.1).unwrap_or(0) + TAIL_US;
    let mut rx = VoiceReceiver::new(48_000).unwrap();
    let t0 = Instant::now() - Duration::from_micros(START);
    let (mut next, mut tick) = (0, START / 10_000);
    let mut out: Vec<f32> = Vec::new();
    let mut frame = vec![0.0f32; 480];
    loop {
        let t = tick * 10_000;
        if t > end_us {
            break;
        }
        let due = t0 + Duration::from_micros(t);
        let now = Instant::now();
        if due > now {
            std::thread::sleep(due - now);
        }
        while next < ev.len() && ev[next].1 <= t {
            let (d, _, p) = &ev[next];
            let now_us = t0.elapsed().as_micros() as u64;
            let _ = rx.push_packet_with_now_mono(p.clone(), PacketArrival { received_at_mono_us: *d }, now_us.max(*d));
            next += 1;
        }
        rx.pull_frame(&mut frame);
        out.extend_from_slice(&frame);
        tick += 1;
    }
    let mut b = Vec::with_capacity(out.len() * 4);
    for v in &out {
        b.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(&a[1], b).unwrap();
    let s = rx.stats();
    let j = format!(
        "{{\n \"receiver\": \"old NetEq VoiceReceiver (56b4c31)\",\n \"neteq_output_samples\": {},\n \"concealed_samples\": {},\n \"concealment_events\": {},\n \"silent_concealed_samples\": {},\n \"talkspurt_flushes\": {},\n \"late_packets_discarded\": {},\n \"inserted_samples_for_deceleration\": {},\n \"removed_samples_for_acceleration\": {},\n \"expand_rate\": {},\n \"packets_pushed\": {}\n}}\n",
        s.neteq_output_samples, s.concealed_samples, s.concealment_events, s.silent_concealed_samples,
        s.talkspurt_flushes, s.late_packets_discarded, s.inserted_samples_for_deceleration,
        s.removed_samples_for_acceleration, s.expand_rate, ev.len()
    );
    std::fs::write(&a[2], j).unwrap();
}
