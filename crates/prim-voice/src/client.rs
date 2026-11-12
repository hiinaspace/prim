//! The voice client: handshake, then the reader, encoder, capture, mixer, output,
//! device-manager and stats threads.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use room_core::rt::Role;
use room_core::voice::voice_state;
use serde_json::Value;

use crate::capture;
use crate::clock::mono_us;
use crate::config::{CaptureSpec, Config, OutputSpec};
use crate::ring::Ring;
use crate::rtio::{handshake, RtConn};
use crate::seats::Seats;
use crate::shared::{ExitReason, GenSync, Senders, Shared, Stats, SAMPLE_RATE};

pub struct Client {
    sh: Arc<Shared>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

fn random_incarnation() -> Result<u32> {
    loop {
        let mut b = [0u8; 4];
        getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("entropy: {e}"))?;
        let n = u32::from_le_bytes(b);
        if n != 0 {
            return Ok(n);
        }
    }
}

impl Client {
    /// Take the `voice` lease over `conn` and start rendering and sending.
    pub fn start(conn: RtConn, cfg: Config) -> Result<Client> {
        let incarnation = match cfg.incarnation {
            Some(i) => i,
            None => random_incarnation()?,
        };
        let mut flags = voice_state::LISTENING;
        if !matches!(cfg.capture, CaptureSpec::None) {
            flags |= voice_state::MIC;
        }
        let lease = handshake(&conn, Role::Voice, cfg.takeover, incarnation, flags)
            .map_err(anyhow::Error::new)
            .context("voice lease")?;
        let generator = capture::generator(&cfg.capture, incarnation)?;
        let seats = Seats::new(lease.self_key, cfg.seats.clone());
        let sh = Arc::new(Shared {
            conn,
            self_key: lease.self_key,
            incarnation,
            start_us: mono_us(),
            stats: Stats::new(),
            senders: Mutex::new(Senders::default()),
            seats: Mutex::new(seats),
            listener: Mutex::new(None),
            tones: Mutex::new(Vec::new()),
            shutdown: AtomicBool::new(false),
            exit: Mutex::new(None),
            out_ring: Ring::new(SAMPLE_RATE as usize), // 0.5 s of stereo
            cap_ring: Ring::new(SAMPLE_RATE as usize * 2),
            gen: GenSync {
                device_gen: AtomicU32::new(0),
                ack: AtomicU32::new(0),
                capture_rate: AtomicU32::new(SAMPLE_RATE),
                reopen: AtomicBool::new(false),
                out_cb_us: AtomicU64::new(0),
                in_cb_us: AtomicU64::new(0),
            },
            primed: AtomicBool::new(false),
            last_send_us: AtomicU64::new(mono_us()),
            cfg,
        });

        let mut threads = Vec::new();
        // Devices first: they decide the capture rate the encoder starts with.
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        {
            let sh2 = sh.clone();
            threads.push(
                std::thread::Builder::new()
                    .name("voice-devices".into())
                    .spawn(move || device_manager(sh2, ready_tx))?,
            );
        }
        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                sh.stop(ExitReason::Stopped);
                for t in threads {
                    let _ = t.join();
                }
                anyhow::bail!("{e}");
            }
            Err(_) => {
                sh.stop(ExitReason::Stopped);
                anyhow::bail!("audio devices did not open within 10 s");
            }
        }
        if let Some(g) = generator {
            threads.push(capture::spawn_synthetic(sh.clone(), g));
        }
        threads.push(crate::send::spawn(sh.clone()));
        threads.push(crate::recv::spawn(sh.clone()));
        threads.push(crate::mixer::spawn(sh.clone()));
        if crate::output::needs_pacer(&sh.cfg.output) {
            threads.push(crate::output::spawn_pacer(sh.clone())?);
        }
        if let Some(path) = sh.cfg.stats_path.clone() {
            let sh2 = sh.clone();
            threads.push(
                std::thread::Builder::new()
                    .name("voice-stats".into())
                    .spawn(move || {
                        let mut next = mono_us() + 1_000_000;
                        while !sh2.is_shutdown() {
                            std::thread::sleep(Duration::from_millis(50));
                            if mono_us() >= next {
                                crate::stats::write(&sh2, &path);
                                next += 1_000_000;
                            }
                        }
                    })?,
            );
        }
        Ok(Client { sh, threads })
    }

    pub fn incarnation(&self) -> u32 {
        self.sh.incarnation
    }

    pub fn self_key(&self) -> room_core::ids::Key {
        self.sh.self_key
    }

    /// Reopen the audio devices (SIGUSR1's job); also flushes the capture path.
    pub fn request_device_change(&self) {
        self.sh.gen.reopen.store(true, Ordering::SeqCst);
    }

    /// Why the client is stopping, once it is.
    pub fn exit_reason(&self) -> Option<ExitReason> {
        *self.sh.exit.lock().unwrap()
    }

    /// The current stats document (what `--stats` writes).
    pub fn stats(&self) -> Value {
        crate::stats::snapshot(&self.sh)
    }

    /// A named counter (`drop_age`, `clock_mismatch`, `fence_stale`, ...).
    pub fn counter(&self, name: &str) -> u64 {
        self.sh.stats.counter(name)
    }

    /// Test and probe access: one peer's NetEq statistics.
    pub fn peer_receiver_stats(
        &self,
        key: &room_core::ids::Key,
    ) -> Option<voice_core::ReceiverStats> {
        let arc = self.sh.senders.lock().unwrap().map.get(key)?.clone();
        let p = arc.lock().unwrap();
        p.receiver.as_ref().map(|r| r.stats())
    }

    pub fn listener_age_p50_us(&self) -> Option<u64> {
        self.sh
            .stats
            .inner
            .lock()
            .unwrap()
            .listener_age_us
            .percentile(50.0)
    }

    /// Stop everything, write the final stats, and return them.
    pub fn stop(self) -> Value {
        let mut this = self;
        this.sh.stop(ExitReason::Stopped);
        let sh = this.sh.clone();
        for t in std::mem::take(&mut this.threads) {
            let _ = t.join();
        }
        if let Some(p) = &sh.cfg.stats_path {
            crate::stats::write(&sh, p);
        }
        crate::stats::snapshot(&sh)
    }
}

/// Owns the cpal streams (which aren't `Send`) and reopens them on a stream error or a
/// device-change request.
fn device_manager(sh: Arc<Shared>, ready: std::sync::mpsc::Sender<Result<(), String>>) {
    let wants_device = matches!(sh.cfg.capture, CaptureSpec::Device)
        || matches!(sh.cfg.output, OutputSpec::Device);
    let mut streams = match crate::device::open(&sh) {
        Ok(s) => Some(s),
        Err(e) => {
            let _ = ready.send(Err(format!("{e:#}")));
            return;
        }
    };
    let _ = ready.send(Ok(()));
    let mut watchdog = Watchdog::new(mono_us());
    while !sh.is_shutdown() {
        std::thread::sleep(Duration::from_millis(20));
        if streams.is_some() && watchdog.stalled(&sh, mono_us()) {
            // The device went away without telling us (a sink unloaded, a graph node
            // gone): reopen exactly as SIGUSR1 does. The process env still holds the
            // routing, so the new streams land where the old ones did.
            sh.stats.bump("device_stall_reopen");
            sh.gen.reopen.store(true, Ordering::SeqCst);
        }
        if !sh.gen.reopen.swap(false, Ordering::SeqCst) {
            continue;
        }
        // Close the old streams first so nothing stale is produced or consumed, then
        // let the encoder flush and acknowledge before new samples can arrive.
        streams = None;
        let g = sh.gen.device_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let deadline = mono_us() + 500_000;
        while sh.gen.ack.load(Ordering::SeqCst) != g && mono_us() < deadline && !sh.is_shutdown() {
            std::thread::sleep(Duration::from_millis(2));
        }
        if wants_device {
            if matches!(sh.cfg.output, OutputSpec::Device) {
                sh.out_ring.clear();
            }
            // Retry until a default device exists again.
            while !sh.is_shutdown() {
                match crate::device::open(&sh) {
                    Ok(s) => {
                        streams = Some(s);
                        watchdog.reset(&sh, mono_us());
                        break;
                    }
                    Err(_) => {
                        sh.stats.bump("device_open_failures");
                        std::thread::sleep(Duration::from_millis(250));
                    }
                }
            }
        }
    }
    drop(streams);
}

/// Device-callback watchdog. A stream that stops calling back for `STALL_US` while it is
/// open is reopened. Callbacks that never started get `FIRST_US` (stream start can be
/// slow). If this thread itself was stopped or starved (SIGSTOP, a stalled host), the
/// callbacks can't be blamed: the baseline restarts.
struct Watchdog {
    opened_us: u64,
    last_tick_us: u64,
}

impl Watchdog {
    const STALL_US: u64 = 300_000;
    const FIRST_US: u64 = 1_500_000;
    const TICK_GAP_US: u64 = 150_000;

    fn new(now: u64) -> Watchdog {
        Watchdog {
            opened_us: now,
            last_tick_us: now,
        }
    }

    /// New streams were just opened: forget the old callbacks.
    fn reset(&mut self, sh: &Shared, now: u64) {
        sh.gen.out_cb_us.store(0, Ordering::Relaxed);
        sh.gen.in_cb_us.store(0, Ordering::Relaxed);
        self.opened_us = now;
        self.last_tick_us = now;
    }

    fn stalled(&mut self, sh: &Shared, now: u64) -> bool {
        let starved = now.saturating_sub(self.last_tick_us) > Self::TICK_GAP_US;
        self.last_tick_us = now;
        if starved {
            self.opened_us = now;
            // The callbacks' own stamps are as old as our pause; start them over.
            sh.gen.out_cb_us.store(0, Ordering::Relaxed);
            sh.gen.in_cb_us.store(0, Ordering::Relaxed);
            return false;
        }
        let stuck = |last: u64| {
            if last == 0 {
                now.saturating_sub(self.opened_us) > Self::FIRST_US
            } else {
                now.saturating_sub(last) > Self::STALL_US
            }
        };
        (matches!(sh.cfg.output, OutputSpec::Device)
            && stuck(sh.gen.out_cb_us.load(Ordering::Relaxed)))
            || (matches!(sh.cfg.capture, CaptureSpec::Device)
                && stuck(sh.gen.in_cb_us.load(Ordering::Relaxed)))
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.sh.stop(ExitReason::Stopped);
    }
}
