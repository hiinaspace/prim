//! The mixer: paced by the output ring (two 10 ms blocks ahead). Per block, per active
//! sender: 480 samples from NetEq, rendered by prim-hrtf at the sender's seat relative
//! to the newest listener pose, summed.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use prim_hrtf::{
    voice_gain, Hrtf, Renderer, RendererConfig, SourceParams, DEFAULT_FAR_M, DEFAULT_NEAR_M,
    DEFAULT_PERCENT,
};
use room_core::ids::Key;
use serde_json::json;

use crate::clock::mono_us;
use crate::seats::local_direction;
use crate::shared::{Shared, BLOCK, BLOCKS_AHEAD, SAMPLE_RATE};
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

/// Seconds without a packet before a sender is released (NetEq reset, slot freed).
const IDLE_US: u64 = 3_000_000;
const TONE_HISTORY: usize = 3600;

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
        }
    }

    /// Render one block into `self.inter` (interleaved stereo) and return it.
    pub fn render_block(&mut self, sh: &Shared) -> &[f32] {
        let now = mono_us();
        self.left.fill(0.0);
        self.right.fill(0.0);

        let pose = *sh.listener.lock().unwrap();
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
            sh.stats.inner.lock().unwrap().listener_age_us.record(age);
            if p.source_mono_us > now {
                sh.stats.bump("listener_stamp_in_future");
            }
        }

        let list: Vec<(Key, _)> = sh
            .senders
            .lock()
            .unwrap()
            .map
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        for (key, arc) in list {
            let mut p = arc.lock().unwrap();
            if !p.active {
                continue;
            }
            if now.saturating_sub(p.last_packet_us) > IDLE_US {
                // Gone quiet: release the slot and the jitter buffer. A later packet
                // (same or new incarnation) starts clean.
                p.active = false;
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
            }
            let Some(rx) = p.receiver.as_mut() else {
                continue;
            };
            rx.pull_frame(&mut self.pull);
            p.blocks_pulled += 1;
            drop(p);

            let seat = sh.seats.lock().unwrap().position(&key);
            let (dir, dist) = local_direction(lpos, lrot, seat);
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
        }

        for i in 0..BLOCK {
            self.inter[2 * i] = self.left[i];
            self.inter[2 * i + 1] = self.right[i];
        }
        if sh.cfg.stats_tone {
            self.probe_tones(sh);
        }
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
            let ahead = BLOCKS_AHEAD * BLOCK;
            while !sh.is_shutdown() {
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
    fn level_match_is_minus_4_7_db_on_top_of_the_distance_law() {
        assert!((level_match() - 0.5821).abs() < 1e-3, "{}", level_match());
        // 150 % volume inside 3 m, scaled; silent at 15 m.
        assert!((source_gain(1.5) - 1.5 * level_match()).abs() < 1e-6);
        assert!((source_gain(9.0) - 0.75 * level_match()).abs() < 1e-5);
        assert_eq!(source_gain(15.0), 0.0);
    }
}
