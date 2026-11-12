//! State shared between the threads. Device callbacks touch only atomics and rings.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
use std::sync::{Arc, Mutex};

use room_core::ids::Key;
use room_core::rt::Pose;
use room_core::voice::IncarnationFence;
use voice_core::VoiceReceiver;

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

pub struct PeerRx {
    pub fence: IncarnationFence,
    pub receiver: Option<VoiceReceiver>,
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
}

impl Default for PeerRx {
    fn default() -> Self {
        Self::new()
    }
}

impl PeerRx {
    pub fn new() -> PeerRx {
        PeerRx {
            fence: IncarnationFence::default(),
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
        }
    }
}

#[derive(Default)]
pub struct Senders {
    /// One lock per sender: the reader and the mixer contend only on the same talker.
    pub map: HashMap<Key, Arc<Mutex<PeerRx>>>,
}

pub struct StatsInner {
    pub ipc_delay_us: Hist,
    pub listener_age_us: Hist,
    pub mixer_block_us: Hist,
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
