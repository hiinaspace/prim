//! A mock daemon on a `socketpair(AF_UNIX, SOCK_SEQPACKET)` speaking `RtMsg`, and
//! talkers that feed it packets in real time.
#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use prim_voice::clock::{mono_us, sleep_until};
use prim_voice::RtConn;
use room_core::ids::Key;
use room_core::rt::{refuse, RtMsg, RT_PROTO};
use room_core::voice::VoiceEnvelope;
use voice_core::{VoiceEncoder, VoiceEncoderConfig, VoicePacket};

pub type Capture = Arc<Mutex<Vec<f32>>>;

pub struct Mock {
    pub conn: Arc<RtConn>,
    /// Messages the client sent, with the mono time the mock read them.
    pub rx: Receiver<(u64, RtMsg)>,
    pub hello: Arc<Mutex<Option<RtMsg>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Mock {
    /// Returns the mock and the client's end of the socket. The mock answers the
    /// first `Hello` with a `Welcome` (or `Refused{busy}` if `refuse_busy`).
    pub fn spawn(self_key: Key, refuse_busy: bool) -> (Mock, RtConn) {
        let (client_end, daemon_end) = RtConn::pair().unwrap();
        let conn = Arc::new(daemon_end);
        let (tx, rx) = channel();
        let hello = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let (c2, h2, s2) = (conn.clone(), hello.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !s2.load(Ordering::SeqCst) {
                let Ok(Some(b)) = c2.recv(Duration::from_millis(20)) else {
                    continue;
                };
                let t = mono_us();
                let m = RtMsg::decode(&b).expect("client sent a valid RtMsg");
                if let RtMsg::Hello { .. } = m {
                    *h2.lock().unwrap() = Some(m);
                    let reply = if refuse_busy {
                        RtMsg::Refused {
                            reason: refuse::BUSY,
                        }
                    } else {
                        RtMsg::Welcome {
                            proto: RT_PROTO,
                            self_key,
                            lease_ttl_ms: 2000,
                            session: 1,
                        }
                    };
                    c2.send(&reply);
                    continue;
                }
                let _ = tx.send((t, m));
            }
        });
        (
            Mock {
                conn,
                rx,
                hello,
                stop,
                thread: Some(thread),
            },
            client_end,
        )
    }

    pub fn send(&self, m: &RtMsg) {
        self.conn.send(m);
    }

    pub fn voice_in(&self, from: Key, received_at: u64, env: VoiceEnvelope) {
        self.send(&RtMsg::VoiceIn {
            from,
            received_at_mono_us: received_at,
            env,
        });
    }

    pub fn drain(&self) -> Vec<(u64, RtMsg)> {
        self.rx.try_iter().collect()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub fn key(b: u8) -> Key {
    Key([b; 32])
}

/// Opus packets for `n` frames of a sine, as the real encoder would send them.
pub fn encode_tone(
    hz: f32,
    amp: f32,
    n: usize,
    first_ts_frames: u32,
    first_seq: u16,
) -> Vec<VoicePacket> {
    let mut enc = VoiceEncoder::new(VoiceEncoderConfig {
        enable_dtx: false,
        ..Default::default()
    })
    .unwrap();
    enc.restore_packet_clock(first_seq, first_ts_frames.wrapping_mul(960));
    let mut out = Vec::new();
    let mut i = 0u64;
    while out.len() < n {
        let pcm: Vec<f32> = (0..960)
            .map(|k| {
                amp * (std::f64::consts::TAU * hz as f64 * (i * 960 + k) as f64 / 48000.0).sin()
                    as f32
            })
            .collect();
        i += 1;
        enc.push_pcm(&pcm);
        while let Some(p) = enc.poll_packet().unwrap() {
            out.push(p);
        }
    }
    out
}

pub fn env(p: &VoicePacket, incarnation: u32, sent: u64) -> VoiceEnvelope {
    prim_voice::send::envelope(p, incarnation, sent).expect("non-empty payload")
}

/// Deterministic jitter source (xorshift), microseconds in [0, max).
pub struct Jitter(u64, pub u64);

impl Jitter {
    pub fn new(seed: u64, max_us: u64) -> Jitter {
        Jitter(seed | 1, max_us)
    }
    pub fn next(&mut self) -> u64 {
        if self.1 == 0 {
            return 0;
        }
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % self.1
    }
}

/// Play `packets` to the client at 20 ms spacing: each is stamped `received_at` the
/// moment it is due, then (with `jitter`) held a random 0..max before it is written,
/// which is the IPC delay the daemon-to-client hop would add.
pub fn play(
    conn: Arc<RtConn>,
    from: Key,
    incarnation: u32,
    packets: Vec<VoicePacket>,
    first_sent_count: u64,
    jitter_max_us: u64,
    seed: u64,
) -> JoinHandle<()> {
    play_with_net(
        conn,
        from,
        incarnation,
        packets,
        first_sent_count,
        0,
        jitter_max_us,
        seed,
    )
}

/// As `play`, with `net_jitter_max_us` of network jitter before the stamp (the packet
/// is due at its slot, reaches the daemon up to that much later, and is stamped then).
#[allow(clippy::too_many_arguments)]
pub fn play_with_net(
    conn: Arc<RtConn>,
    from: Key,
    incarnation: u32,
    packets: Vec<VoicePacket>,
    first_sent_count: u64,
    net_jitter_max_us: u64,
    jitter_max_us: u64,
    seed: u64,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut j = Jitter::new(seed, jitter_max_us);
        let mut nj = Jitter::new(seed ^ 0x5555, net_jitter_max_us);
        let t0 = mono_us() + 5_000;
        for (i, p) in packets.iter().enumerate() {
            sleep_until(t0 + i as u64 * 20_000 + nj.next());
            let stamp = mono_us();
            let d = j.next();
            if d > 0 {
                sleep_until(stamp + d);
            }
            conn.send(&RtMsg::VoiceIn {
                from,
                received_at_mono_us: stamp,
                env: env(p, incarnation, first_sent_count + i as u64),
            });
        }
    })
}

pub fn amp_l_r(buf: &[f32], from_frame: usize, frames: usize, hz: f32) -> (f32, f32) {
    let seg = &buf[from_frame * 2..(from_frame + frames) * 2];
    let l: Vec<f32> = seg.iter().step_by(2).copied().collect();
    let r: Vec<f32> = seg.iter().skip(1).step_by(2).copied().collect();
    (
        prim_voice::tone::goertzel_amp(&l, hz, 48000.0),
        prim_voice::tone::goertzel_amp(&r, hz, 48000.0),
    )
}
