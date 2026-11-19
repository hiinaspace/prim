//! State shared between the threads. Device callbacks touch only atomics and rings.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize};
use std::sync::{Arc, Mutex};

use room_core::ids::Key;
use room_core::rt::Pose;
use room_core::voice::IncarnationFence;
use voice_core::PlayoutReceiver;

use crate::config::Config;
use crate::hist::{AtomicHist, Hist};
use crate::ring::Ring;
use crate::rtio::RtConn;
use crate::seats::Seats;

/// Frames per mixer block (10 ms at 48 kHz).
pub const BLOCK: usize = 480;
pub const SAMPLE_RATE: u32 = 48_000;
/// The mixer keeps this many blocks ahead of the output.
pub const BLOCKS_AHEAD: usize = 2;
/// ...and never more than this many, whatever the device asks for.
pub const BLOCKS_AHEAD_MAX: usize = 8;

/// Blocks the mixer keeps rendered ahead of an output device that pulls `burst` frames at once.
/// The ring must hold a whole pull, or the device gets a partial buffer and zeros: a 1024-frame
/// quantum (two callbacks of 512 back to back, or one of 1024) underran every other pull with
/// the fixed two blocks (960 frames).
pub fn blocks_ahead(burst: usize) -> usize {
    (burst * 5 / 4).div_ceil(BLOCK).clamp(BLOCKS_AHEAD, BLOCKS_AHEAD_MAX)
}

/// Largest output pull (frames) seen in the last 5 to 10 seconds. A plain running maximum only
/// ever rises: one stall that makes the device ask for a big burst once would keep the ring
/// deep, and so the playout latency high, for the rest of the process.
#[derive(Debug, Default)]
pub struct BurstWindow {
    epoch_start_us: u64,
    current: usize,
    previous: usize,
}

impl BurstWindow {
    pub const EPOCH_US: u64 = 5_000_000;

    /// Record a pull of `burst` frames at `now_us`; returns the maximum over the current and the
    /// previous 5 s epoch.
    pub fn record(&mut self, now_us: u64, burst: usize) -> usize {
        let elapsed = now_us.saturating_sub(self.epoch_start_us);
        if self.epoch_start_us == 0 || elapsed >= 2 * Self::EPOCH_US {
            self.previous = 0;
            self.current = 0;
            self.epoch_start_us = now_us;
        } else if elapsed >= Self::EPOCH_US {
            self.previous = self.current;
            self.current = 0;
            self.epoch_start_us += Self::EPOCH_US;
        }
        self.current = self.current.max(burst);
        self.current.max(self.previous)
    }
}

#[cfg(test)]
mod burst_tests {
    use super::*;

    #[test]
    fn a_burst_ages_out_of_the_window() {
        let mut w = BurstWindow::default();
        let s = 1_000_000;
        assert_eq!(w.record(s, 480), 480);
        assert_eq!(w.record(2 * s, 2048), 2048, "a one-off big pull counts at once");
        assert_eq!(w.record(7 * s, 480), 2048, "...and still for the next epoch");
        assert_eq!(w.record(13 * s, 480), 480, "...but not for ever");
        // Left alone for a long time: back to what the device asks for now.
        assert_eq!(w.record(100 * s, 512), 512);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitReason {
    /// Asked to stop (signal, `stop()`).
    Stopped,
    /// The daemon revoked the lease (`room_core::rt::revoke`).
    Revoked(u8),
    /// The realtime socket closed.
    SocketClosed,
}

#[derive(Clone, Copy, Debug)]
pub struct ListenerState {
    pub source_mono_us: u64,
    pub forwarded_mono_us: u64,
    pub seq: u32,
    pub pose: Pose,
}

/// What the rt reader hands the mixer for one sender, in arrival order.
pub enum InEntry {
    /// A new incarnation: reset the receiver (and the renderer slot) before what follows.
    Reset { incarnation: u32 },
    Packet {
        pkt: voice_core::VoicePacket,
        obs: voice_core::PacketObservation,
    },
}

/// Longest inbox kept (5 s of packets); beyond it the oldest packet is dropped and counted.
pub const INBOX_MAX: usize = 256;

pub struct InboxState {
    pub fence: IncarnationFence,
    pub entries: Vec<InEntry>,
    pub stale: u64,
    pub overflow: u64,
}

/// One sender's queue between the rt reader and the mixer. Its own tiny lock: the reader only
/// ever waits for a `Vec::push`, never behind decode, time-stretch or HRTF work, which the mixer
/// does under the sender's `PeerRx` lock.
pub struct Inbox {
    pub state: Mutex<InboxState>,
}

impl Default for Inbox {
    fn default() -> Self {
        Inbox {
            state: Mutex::new(InboxState {
                fence: IncarnationFence::default(),
                entries: Vec::with_capacity(32),
                stale: 0,
                overflow: 0,
            }),
        }
    }
}

pub struct PeerRx {
    pub inbox: Arc<Inbox>,
    /// Swapped with the inbox's entries on each drain, so draining allocates nothing.
    pub scratch: Vec<InEntry>,
    pub receiver: Option<PlayoutReceiver>,
    pub incarnation: u32,
    pub slot: Option<usize>,
    pub last_packet_us: u64,
    /// The mixer must reset this sender's renderer slot before its next block.
    pub needs_reset: bool,
    pub active: bool,
    pub packets_accepted: u64,
    pub new_streams: u64,
    pub stale: u64,
    pub push_errors: u64,
    pub blocks_pulled: u64,
    pub blocks_skipped: u64,
    /// The mixer's idle-skip state for this sender.
    pub gate: crate::mixer::IdleGate,
}

impl Default for PeerRx {
    fn default() -> Self {
        Self::new()
    }
}

impl PeerRx {
    pub fn new() -> PeerRx {
        Self::with_inbox(Arc::new(Inbox::default()))
    }

    pub fn with_inbox(inbox: Arc<Inbox>) -> PeerRx {
        PeerRx {
            inbox,
            scratch: Vec::with_capacity(32),
            receiver: None,
            incarnation: 0,
            slot: None,
            last_packet_us: 0,
            needs_reset: false,
            active: false,
            packets_accepted: 0,
            new_streams: 0,
            stale: 0,
            push_errors: 0,
            blocks_pulled: 0,
            blocks_skipped: 0,
            gate: Default::default(),
        }
    }
}

#[derive(Default)]
pub struct Senders {
    /// One lock per sender: the reader and the mixer contend only on the same talker.
    pub map: HashMap<Key, Arc<Mutex<PeerRx>>>,
    /// The same senders' inboxes, for the reader (which must not take a `PeerRx` lock).
    pub inboxes: HashMap<Key, Arc<Inbox>>,
}

pub struct StatsInner {
    pub ipc_delay_us: Hist,
    pub listener_age_us: Hist,
    pub mixer_block_us: Hist,
    /// Per-block totals over all senders of each mixer stage, in microseconds.
    pub mixer_lock_us: Hist,
    /// How long the rt reader waited to take a sender's receiver lock, per packet.
    pub reader_peer_wait_us: Hist,
    /// How long the mixer held a sender's receiver lock (drain, pull, level control), per block.
    pub mixer_peer_hold_us: Hist,
    pub mixer_pull_us: Hist,
    pub mixer_hrtf_us: Hist,
    pub mixer_sum_us: Hist,
    pub counters: BTreeMap<&'static str, u64>,
    pub tone_seconds: VecDeque<serde_json::Value>,
}

pub struct Stats {
    pub inner: Mutex<StatsInner>,
    pub underruns: AtomicU64,
    pub callback_gap_us: AtomicHist,
}

impl Default for Stats {
    fn default() -> Self {
        Self::new()
    }
}

impl Stats {
    pub fn new() -> Stats {
        Stats {
            inner: Mutex::new(StatsInner {
                ipc_delay_us: Hist::default(),
                listener_age_us: Hist::default(),
                mixer_block_us: Hist::default(),
                mixer_lock_us: Hist::default(),
                reader_peer_wait_us: Hist::default(),
                mixer_peer_hold_us: Hist::default(),
                mixer_pull_us: Hist::default(),
                mixer_hrtf_us: Hist::default(),
                mixer_sum_us: Hist::default(),
                counters: BTreeMap::new(),
                tone_seconds: VecDeque::new(),
            }),
            underruns: AtomicU64::new(0),
            callback_gap_us: AtomicHist::default(),
        }
    }

    pub fn bump(&self, name: &'static str) {
        self.add(name, 1);
    }

    pub fn add(&self, name: &'static str, n: u64) {
        *self.inner.lock().unwrap().counters.entry(name).or_insert(0) += n;
    }

    pub fn counter(&self, name: &str) -> u64 {
        self.inner
            .lock()
            .unwrap()
            .counters
            .get(name)
            .copied()
            .unwrap_or(0)
    }
}

/// Device-change handshake between the device manager and the encoder thread.
pub struct GenSync {
    /// Bumped on every device reopen (also reported as `device_generation`).
    pub device_gen: AtomicU32,
    /// The encoder's acknowledgement: it has flushed the capture ring for this generation.
    pub ack: AtomicU32,
    /// Sample rate of the capture ring's current source.
    pub capture_rate: AtomicU32,
    /// Set by SIGUSR1 or a stream error; the device manager reopens.
    pub reopen: AtomicBool,
    /// `mono_us` of the latest output / input device callback (0: none yet on this
    /// stream). The device manager's watchdog reads these.
    pub out_cb_us: AtomicU64,
    pub in_cb_us: AtomicU64,
}

pub struct Shared {
    pub cfg: Config,
    pub conn: RtConn,
    pub self_key: Key,
    pub incarnation: u32,
    pub start_us: u64,
    pub stats: Stats,
    pub senders: Mutex<Senders>,
    pub seats: Mutex<Seats>,
    pub listener: Mutex<Option<ListenerState>>,
    /// (incarnation, hz) of every `tone:auto`-style stream seen, for the tone stats.
    pub tones: Mutex<Vec<(u32, f32)>>,
    pub shutdown: AtomicBool,
    pub exit: Mutex<Option<ExitReason>>,
    pub out_ring: Arc<Ring>,
    pub cap_ring: Arc<Ring>,
    pub gen: GenSync,
    /// The mixer has filled the output ring once; before that, shortfalls aren't underruns.
    pub primed: AtomicBool,
    /// The most frames the output device has asked for in one burst (callbacks less than 2 ms
    /// apart add up). The mixer keeps at least this much (plus a quarter) rendered ahead.
    pub out_burst: AtomicUsize,
    pub last_send_us: AtomicU64,
}

impl Shared {
    pub fn stop(&self, why: ExitReason) {
        let mut e = self.exit.lock().unwrap();
        if e.is_none() {
            *e = Some(why);
        }
        self.shutdown
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(std::sync::atomic::Ordering::Relaxed)
    }
}
