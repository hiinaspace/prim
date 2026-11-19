//! The mixer: paced by the output ring (two 10 ms blocks ahead). Per block, per active
//! sender: 480 samples from the playout receiver, rendered by prim-hrtf at the sender's seat relative
//! to the newest listener pose, summed.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use prim_hrtf::{
    voice_gain, Hrtf, Renderer, RendererConfig, SourceParams, DEFAULT_FAR_M, DEFAULT_NEAR_M,
    DEFAULT_PERCENT,
};
use room_core::ids::Key;
use serde_json::json;

use crate::clock::mono_us;
use crate::seats::local_direction;
use crate::shared::{blocks_ahead, Shared, BLOCK, SAMPLE_RATE};
use crate::tone::Goertzel;

/// CIPIC subject 124 baked by prim-hrtf (attribution: crates/prim-voice/README.md).
pub static HRTF_BYTES: &[u8] = include_bytes!("../data/cipic_124.phrtf");

/// prim-hrtf renders 4.7 dB louder than the Godot/Steam Audio path at equal gain
/// (measured in round R2a, prim-hrtf `results/r2a-20261111.json`). Scaling by this
/// puts the two on the same level, so the voice volume setting means the same thing.
/// The only place the constant lives.
pub const LEVEL_MATCH_DB: f32 = -4.7;

pub fn level_match() -> f32 {
    10f32.powf(LEVEL_MATCH_DB / 20.0)
}

/// Linear gain for a sender `distance` metres away: Prim's distance law, level matched.
pub fn source_gain(distance: f32) -> f32 {
    voice_gain(distance, DEFAULT_PERCENT, DEFAULT_NEAR_M, DEFAULT_FAR_M) * level_match()
}

/// Seconds without a packet before a sender is released (receiver reset, slot freed).
const IDLE_US: u64 = 3_000_000;
const TONE_HISTORY: usize = 3600;

/// What the mixer does with one sender's block, from the receiver's idle state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Pull and render as usual.
    Render,
    /// The receiver is idle but the HRTF filter still holds the end of the speech: render
    /// digital zero through it, without pulling.
    Tail,
    /// Idle and the filter has run out: the renderer source was just reset; skip from now.
    Park,
    /// Already parked: no pull, no decode, no HRTF.
    Skip,
}

/// Per-sender gate for the idle skip. Pulling an idle receiver returns zeros, and the
/// HRTF of zeros is zeros once the filter's memory (`interp_len - 1` samples) has been
/// flushed, so after that tail the sender costs nothing until a packet wakes it.
#[derive(Clone, Copy, Debug, Default)]
pub struct IdleGate {
    parked: bool,
    tail_left: Option<u32>,
}

impl IdleGate {
    pub fn step(&mut self, idle: bool, tail_blocks: u32) -> Step {
        if !idle {
            self.parked = false;
            self.tail_left = None;
            return Step::Render;
        }
        if self.parked {
            return Step::Skip;
        }
        let left = self.tail_left.unwrap_or(tail_blocks);
        if left > 0 {
            self.tail_left = Some(left - 1);
            return Step::Tail;
        }
        self.parked = true;
        Step::Park
    }

    /// Forget the state (the slot was released or reset).
    pub fn reset(&mut self) {
        *self = IdleGate::default();
    }
}

/// Blocks of zero input that flush an HRTF FIR with `interp_len` taps.
pub fn tail_blocks(interp_len: usize) -> u32 {
    interp_len.saturating_sub(1).div_ceil(BLOCK) as u32
}

/// Lock `m`, adding the wait to `acc_ns`.
fn timed<'a, T>(m: &'a Mutex<T>, acc_ns: &mut u64) -> MutexGuard<'a, T> {
    let t = Instant::now();
    let g = m.lock().unwrap();
    *acc_ns += t.elapsed().as_nanos() as u64;
    g
}

struct ToneProbe {
    incarnation: u32,
    hz: f32,
    l: Goertzel,
    r: Goertzel,
}

pub struct Mixer {
    renderer: Renderer,
    slots: Vec<Option<Key>>,
    pull: Vec<f32>,
    left: Vec<f32>,
    right: Vec<f32>,
    inter: Vec<f32>,
    probes: Vec<ToneProbe>,
    blocks_this_second: u32,
    seconds: u64,
    tail_blocks: u32,
    /// Output gain of the deafen fade: 1 hearing, 0 deaf.
    deaf_gain: f32,
    deaf_flushed: bool,
}

/// Scale a stereo block by a gain that moves linearly from `g0` to `g1` across it (one
/// block, 10 ms: the deafen and undeafen fade).
pub fn apply_gain_ramp(left: &mut [f32], right: &mut [f32], g0: f32, g1: f32) {
    let n = left.len();
    for i in 0..n {
        let g = g0 + (g1 - g0) * (i + 1) as f32 / n as f32;
        left[i] *= g;
        right[i] *= g;
    }
}

impl Default for Mixer {
    fn default() -> Self {
        Self::new()
    }
}

impl Mixer {
    pub fn new() -> Mixer {
        let hrtf = Hrtf::from_bytes(HRTF_BYTES).expect("bundled HRTF");
        let cfg = RendererConfig {
            sample_rate: SAMPLE_RATE,
            block: BLOCK,
            ..RendererConfig::default()
        };
        let renderer = Renderer::new(hrtf, cfg).expect("renderer");
        let slots = vec![None; renderer.max_sources()];
        let tail_blocks = tail_blocks(renderer.hrtf().interp_len());
        Mixer {
            renderer,
            slots,
            pull: vec![0.0; BLOCK],
            left: vec![0.0; BLOCK],
            right: vec![0.0; BLOCK],
            inter: vec![0.0; BLOCK * 2],
            probes: vec![],
            blocks_this_second: 0,
            seconds: 0,
            tail_blocks,
            deaf_gain: 1.0,
            deaf_flushed: false,
        }
    }

    /// Render one block into `self.inter` (interleaved stereo) and return it. Stage times
    /// of the block (lock waits, pull/decode, HRTF, sum) go to the stats histograms.
    pub fn render_block(&mut self, sh: &Shared) -> &[f32] {
        let now = mono_us();
        // The control version this block honours (read first, stored last).
        let ctl_version = sh.ctl.version.load(Ordering::SeqCst);
        let deaf_target = sh.ctl.deaf.load(Ordering::SeqCst);
        let goal = if deaf_target { 0.0 } else { 1.0 };
        let g0 = self.deaf_gain;
        let g1 = goal;
        // Fully deaf (and staying so): keep draining and pulling so the playout cursors stay
        // current, but spend nothing on HRTF.
        let skip_render = g0 == 0.0 && g1 == 0.0;
        if skip_render && !self.deaf_flushed {
            // Rendering stops here: forget every source's filter history once, so the first
            // block after undeafening starts clean (the fade-in hides the restart).
            for (i, s) in self.slots.iter().enumerate() {
                if s.is_some() {
                    self.renderer.reset_source(i);
                }
            }
            self.deaf_flushed = true;
        } else if !skip_render {
            self.deaf_flushed = false;
        }
        let (mut lock_ns, mut pull_ns, mut hrtf_ns) = (0u64, 0u64, 0u64);
        self.left.fill(0.0);
        self.right.fill(0.0);

        let pose = *timed(&sh.listener, &mut lock_ns);
        let (lpos, lrot) = match pose {
            Some(p) => (p.pose.pos, p.pose.rot),
            None => (
                [0.0, crate::seats::HEAD_HEIGHT_M, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ),
        };
        if let Some(p) = pose {
            // The pose's own source stamp, to now: how stale the head is at this block.
            let age = now.saturating_sub(p.source_mono_us);
            timed(&sh.stats.inner, &mut lock_ns)
                .listener_age_us
                .record(age);
            if p.source_mono_us > now {
                sh.stats.bump("listener_stamp_in_future");
            }
        }

        let list: Vec<(Key, _)> = timed(&sh.senders, &mut lock_ns)
            .map
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        let (mut rendered, mut tail, mut skipped) = (0u64, 0u64, 0u64);
        let mut hold_ns: Vec<u64> = Vec::new();
        for (key, arc) in list {
            let mut p = timed(&arc, &mut lock_ns);
            let t_hold = Instant::now();
            crate::recv::drain_inbox(sh, &mut p);
            if !p.active {
                continue;
            }
            if now.saturating_sub(p.last_packet_us) > IDLE_US {
                // Gone quiet: release the slot and the jitter buffer. A later packet
                // (same or new incarnation) starts clean.
                p.active = false;
                p.gate.reset();
                if let Some(s) = p.slot.take() {
                    self.renderer.reset_source(s);
                    self.slots[s] = None;
                }
                if let Some(r) = p.receiver.as_mut() {
                    let _ = r.reset_stream();
                }
                continue;
            }
            let slot = match p.slot {
                Some(s) => s,
                None => match self.slots.iter().position(|s| s.is_none()) {
                    Some(s) => {
                        self.slots[s] = Some(key);
                        self.renderer.reset_source(s);
                        p.slot = Some(s);
                        p.needs_reset = false;
                        p.gate.reset();
                        s
                    }
                    None => {
                        sh.stats.bump("drop_no_slot");
                        continue;
                    }
                },
            };
            if p.needs_reset {
                self.renderer.reset_source(slot);
                p.needs_reset = false;
                p.gate.reset();
            }
            let t_pull = Instant::now();
            let step = if sh.cfg.test_no_idle_skip {
                Step::Render
            } else {
                let idle = p.receiver.as_ref().is_some_and(|r| r.is_idle());
                p.gate.step(idle, self.tail_blocks)
            };
            match step {
                Step::Skip => {
                    p.blocks_skipped += 1;
                    skipped += 1;
                    continue;
                }
                Step::Park => {
                    // The filter is flushed: forget the slot's history and gain, so the next
                    // talkspurt starts like a fresh source.
                    self.renderer.reset_source(slot);
                    p.blocks_skipped += 1;
                    skipped += 1;
                    continue;
                }
                Step::Tail => {
                    self.pull.fill(0.0);
                    tail += 1;
                }
                Step::Render => {
                    let Some(rx) = p.receiver.as_mut() else {
                        continue;
                    };
                    rx.pull(&mut self.pull);
                    p.blocks_pulled += 1;
                    rendered += 1;
                }
            }
            pull_ns += t_pull.elapsed().as_nanos() as u64;
            drop(p);
            hold_ns.push(t_hold.elapsed().as_micros() as u64);
            if skip_render {
                continue;
            }

            let seat = timed(&sh.seats, &mut lock_ns).position(&key);
            let (dir, dist) = local_direction(lpos, lrot, seat);
            let t_hrtf = Instant::now();
            self.renderer.render_source(
                slot,
                &self.pull,
                SourceParams {
                    direction: dir,
                    gain: source_gain(dist),
                },
                &mut self.left,
                &mut self.right,
            );
            hrtf_ns += t_hrtf.elapsed().as_nanos() as u64;
        }

        let t_sum = Instant::now();
        if g0 != 1.0 || g1 != 1.0 {
            apply_gain_ramp(&mut self.left, &mut self.right, g0, g1);
        }
        self.deaf_gain = g1;
        for i in 0..BLOCK {
            self.inter[2 * i] = self.left[i];
            self.inter[2 * i + 1] = self.right[i];
        }
        if sh.cfg.stats_tone {
            self.probe_tones(sh);
        }
        let sum_ns = t_sum.elapsed().as_nanos() as u64;
        let mut g = sh.stats.inner.lock().unwrap();
        g.mixer_lock_us.record(lock_ns / 1000);
        for h in hold_ns {
            g.mixer_peer_hold_us.record(h);
        }
        g.mixer_pull_us.record(pull_ns / 1000);
        g.mixer_hrtf_us.record(hrtf_ns / 1000);
        g.mixer_sum_us.record(sum_ns / 1000);
        *g.counters.entry("mixer_peer_blocks_rendered").or_insert(0) += rendered;
        *g.counters.entry("mixer_peer_blocks_tail").or_insert(0) += tail;
        *g.counters.entry("mixer_peer_blocks_skipped").or_insert(0) += skipped;
        drop(g);
        sh.ctl.mix_seen.store(ctl_version, Ordering::SeqCst);
        &self.inter
    }

    /// Goertzel energy per output second at each tone seen so far.
    fn probe_tones(&mut self, sh: &Shared) {
        {
            let tones = sh.tones.lock().unwrap();
            for (inc, hz) in tones.iter() {
                if !self.probes.iter().any(|p| p.incarnation == *inc) {
                    self.probes.push(ToneProbe {
                        incarnation: *inc,
                        hz: *hz,
                        l: Goertzel::new(*hz, SAMPLE_RATE as f32),
                        r: Goertzel::new(*hz, SAMPLE_RATE as f32),
                    });
                }
            }
        }
        for p in self.probes.iter_mut() {
            for i in 0..BLOCK {
                p.l.push(self.left[i]);
                p.r.push(self.right[i]);
            }
        }
        self.blocks_this_second += 1;
        if self.blocks_this_second as usize == SAMPLE_RATE as usize / BLOCK {
            let tones: Vec<_> = self
                .probes
                .iter()
                .map(|p| json!({"incarnation": p.incarnation, "hz": p.hz, "l": p.l.amplitude(), "r": p.r.amplitude()}))
                .collect();
            let mut g = sh.stats.inner.lock().unwrap();
            g.tone_seconds
                .push_back(json!({"second": self.seconds, "tones": tones}));
            while g.tone_seconds.len() > TONE_HISTORY {
                g.tone_seconds.pop_front();
            }
            drop(g);
            for p in self.probes.iter_mut() {
                p.l.reset();
                p.r.reset();
            }
            self.blocks_this_second = 0;
            self.seconds += 1;
        }
    }
}

pub fn spawn(sh: Arc<Shared>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("voice-mixer".into())
        .spawn(move || {
            let mut m = Mixer::new();
            while !sh.is_shutdown() {
                let ahead = blocks_ahead(sh.out_burst.load(Ordering::Relaxed)) * BLOCK;
                let fill = sh.out_ring.len() / 2;
                if fill >= ahead {
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                }
                let t0 = mono_us();
                let block = m.render_block(&sh);
                sh.out_ring.push(block);
                sh.stats
                    .inner
                    .lock()
                    .unwrap()
                    .mixer_block_us
                    .record(mono_us() - t0);
                if fill + BLOCK >= ahead {
                    sh.primed.store(true, Ordering::SeqCst);
                }
            }
        })
        .expect("spawn mixer")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deafen_ramp_is_linear_over_one_block() {
        let mut l = vec![1.0f32; BLOCK];
        let mut r = vec![-0.5f32; BLOCK];
        apply_gain_ramp(&mut l, &mut r, 1.0, 0.0);
        assert!((l[0] - (1.0 - 1.0 / BLOCK as f32)).abs() < 1e-6);
        assert_eq!(l[BLOCK - 1], 0.0, "silent at the block end");
        assert!((r[BLOCK / 2 - 1] + 0.5 * 0.5).abs() < 2e-3);
        let worst = l.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        assert!(worst <= 1.0 / BLOCK as f32 + 1e-6, "step {worst}");
        // Back up the other way.
        let mut l = vec![1.0f32; BLOCK];
        let mut r = vec![1.0f32; BLOCK];
        apply_gain_ramp(&mut l, &mut r, 0.0, 1.0);
        assert!((l[BLOCK - 1] - 1.0).abs() < 1e-6 && l[0] < 0.01);
    }

    #[test]
    fn ring_depth_covers_the_devices_pull() {
        use crate::shared::blocks_ahead as b;
        assert_eq!(b(0), 2); // nothing seen yet
        assert_eq!(b(256), 2);
        assert_eq!(b(512), 2); // the default 512 quantum keeps R0's depth
        assert_eq!(b(960), 3); // two blocks would leave no margin at all
        assert_eq!(b(1024), 3); // quantum 1024: 1440 frames for a 1024 pull
        assert_eq!(b(2048), 6);
        assert_eq!(b(100_000), 8);
        for burst in [512usize, 1024, 1536, 2048] {
            assert!(b(burst) * BLOCK >= burst + BLOCK / 4 || b(burst) == 8, "{burst}");
        }
    }

    #[test]
    fn level_match_is_minus_4_7_db_on_top_of_the_distance_law() {
        assert!((level_match() - 0.5821).abs() < 1e-3, "{}", level_match());
        // 150 % volume inside 3 m, scaled; silent at 15 m.
        assert!((source_gain(1.5) - 1.5 * level_match()).abs() < 1e-6);
        assert!((source_gain(9.0) - 0.75 * level_match()).abs() < 1e-5);
        assert_eq!(source_gain(15.0), 0.0);
    }

    fn renderer() -> Renderer {
        let hrtf = Hrtf::from_bytes(HRTF_BYTES).unwrap();
        Renderer::new(
            hrtf,
            RendererConfig {
                sample_rate: SAMPLE_RATE,
                block: BLOCK,
                ..RendererConfig::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn the_hrtf_tail_is_one_block_and_the_gate_walks_tail_park_skip_render() {
        let r = renderer();
        let tb = tail_blocks(r.hrtf().interp_len());
        assert_eq!(tb as usize, (r.hrtf().interp_len() - 1).div_ceil(BLOCK));
        assert!(tb >= 1);
        let mut g = IdleGate::default();
        assert_eq!(g.step(false, 2), Step::Render);
        assert_eq!(g.step(true, 2), Step::Tail);
        assert_eq!(g.step(true, 2), Step::Tail);
        assert_eq!(g.step(true, 2), Step::Park);
        assert_eq!(g.step(true, 2), Step::Skip);
        assert_eq!(g.step(true, 2), Step::Skip);
        assert_eq!(g.step(false, 2), Step::Render);
        // idle again: a fresh tail
        assert_eq!(g.step(true, 2), Step::Tail);
    }

    /// One sender, a noise burst, silence, a second burst; rendered always, and rendered
    /// through the gate (tail blocks, then reset and skip). The skip must not change a
    /// single sample: the tail after speech ends has the same energy within 0.1 dB (here:
    /// identical to float noise), and the next burst starts the same.
    #[test]
    fn skipping_an_idle_peer_does_not_truncate_the_hrtf_tail() {
        let burst = 30usize;
        let quiet = 25usize;
        let total = burst + quiet + burst;
        let mut rng = 0x1234_5678u32;
        let mut noise = move || {
            rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
            (rng >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
        };
        let input: Vec<Vec<f32>> = (0..total)
            .map(|b| {
                if b < burst || b >= burst + quiet {
                    (0..BLOCK).map(|_| 0.3 * noise()).collect()
                } else {
                    vec![0.0; BLOCK]
                }
            })
            .collect();
        let params = SourceParams {
            direction: [0.7, 0.1, -0.7],
            gain: 0.8,
        };
        let run = |gated: bool| -> (Vec<f32>, usize) {
            let mut r = renderer();
            let tb = tail_blocks(r.hrtf().interp_len());
            let mut gate = IdleGate::default();
            let mut out = vec![0.0f32; total * BLOCK * 2];
            let mut rendered = 0;
            for (b, inp) in input.iter().enumerate() {
                let idle = gated && inp.iter().all(|v| *v == 0.0);
                let (mut l, mut rr) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
                match gate.step(idle, tb) {
                    Step::Render | Step::Tail => {
                        r.render_source(0, inp, params, &mut l, &mut rr);
                        rendered += 1;
                    }
                    Step::Park => r.reset_source(0),
                    Step::Skip => {}
                }
                for i in 0..BLOCK {
                    out[(b * BLOCK + i) * 2] = l[i];
                    out[(b * BLOCK + i) * 2 + 1] = rr[i];
                }
            }
            (out, rendered)
        };
        let (full, n_full) = run(false);
        let (gated, n_gated) = run(true);
        assert_eq!(n_full, total);
        assert!(n_gated < n_full - quiet + 3, "skipped {}", n_full - n_gated);
        let energy = |x: &[f32]| x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>();
        let tail = (burst * BLOCK * 2)..((burst + 1) * BLOCK * 2);
        let (e_full, e_gated) = (energy(&full[tail.clone()]), energy(&gated[tail]));
        assert!(e_full > 1e-6, "the tail block has the filter's ringing");
        let db = 10.0 * (e_gated / e_full).log10();
        assert!(db.abs() < 0.1, "tail energy differs by {db} dB");
        let worst = full
            .iter()
            .zip(&gated)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 1e-4, "gated render differs from the full render by {worst}");
    }
}
