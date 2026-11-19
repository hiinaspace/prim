//! Test-only network faults applied to outgoing media packets (the R0 oracle harness
//! sets them with `--test-drop-every`, `--test-drop-burst`, `--test-reorder`). They sit
//! in the sender because the harness's talkers are real `prim voice` processes; they
//! change nothing on the wire format, only which packets reach the daemon and in what
//! order. End markers (empty payload) always pass straight through. Never set in
//! product use.

use voice_core::VoicePacket;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FaultConfig {
    /// Drop every Nth media packet.
    pub drop_every: Option<u64>,
    /// `(n, m)`: of every n media packets, drop m consecutive ones (the last m of the cycle).
    pub drop_burst: Option<(u64, u64)>,
    /// `(n, k)`: hold every nth media packet and release it after k more have gone out.
    pub reorder: Option<(u64, u64)>,
}

impl FaultConfig {
    pub fn any(&self) -> bool {
        self.drop_every.is_some() || self.drop_burst.is_some() || self.reorder.is_some()
    }
}

#[derive(Default)]
pub struct FaultState {
    cfg: FaultConfig,
    seen: u64,
    /// (release when `released_after` reaches this, packet)
    held: Vec<(u64, VoicePacket)>,
    sent_media: u64,
    pub dropped: u64,
    pub reordered: u64,
}

impl FaultState {
    pub fn new(cfg: FaultConfig) -> FaultState {
        FaultState {
            cfg,
            ..Default::default()
        }
    }

    /// The packets to put on the wire now, in order, for one packet the encoder produced.
    pub fn apply(&mut self, p: VoicePacket) -> Vec<VoicePacket> {
        if !self.cfg.any() || p.payload.is_empty() {
            return vec![p];
        }
        self.seen += 1;
        let n = self.seen;
        let drop_every = self.cfg.drop_every.is_some_and(|e| n % e == 0);
        let drop_burst = self
            .cfg
            .drop_burst
            .is_some_and(|(cycle, m)| (n - 1) % cycle >= cycle.saturating_sub(m));
        if drop_every || drop_burst {
            self.dropped += 1;
            return self.release(vec![]);
        }
        if let Some((every, k)) = self.cfg.reorder {
            if n % every == 0 {
                self.reordered += 1;
                self.held.push((self.sent_media + k.max(1), p));
                return vec![];
            }
        }
        self.release(vec![p])
    }

    /// Every packet still held (at a clean exit), in the order they were held.
    pub fn flush(&mut self) -> Vec<VoicePacket> {
        self.held.drain(..).map(|(_, p)| p).collect()
    }

    /// Append the held packets that are due after `out` went out.
    fn release(&mut self, mut out: Vec<VoicePacket>) -> Vec<VoicePacket> {
        self.sent_media += out.len() as u64;
        let mut i = 0;
        while i < self.held.len() {
            if self.held[i].0 <= self.sent_media {
                out.push(self.held.remove(i).1);
            } else {
                i += 1;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use voice_core::PacketFlags;

    fn pkt(seq: u16) -> VoicePacket {
        VoicePacket {
            seq,
            timestamp: seq as u32 * 960,
            flags: PacketFlags::from_bits(0),
            payload: vec![1],
        }
    }

    fn run(cfg: FaultConfig, n: u16) -> Vec<u16> {
        let mut s = FaultState::new(cfg);
        (0..n).flat_map(|i| s.apply(pkt(i))).map(|p| p.seq).collect()
    }

    #[test]
    fn drop_every_and_burst() {
        let v = run(
            FaultConfig {
                drop_every: Some(4),
                ..Default::default()
            },
            12,
        );
        assert_eq!(v, vec![0, 1, 2, 4, 5, 6, 8, 9, 10]);
        let v = run(
            FaultConfig {
                drop_burst: Some((5, 2)),
                ..Default::default()
            },
            10,
        );
        assert_eq!(v, vec![0, 1, 2, 5, 6, 7]);
    }

    #[test]
    fn reorder_swaps_and_delays() {
        let swap = run(
            FaultConfig {
                reorder: Some((5, 1)),
                ..Default::default()
            },
            10,
        );
        assert_eq!(swap, vec![0, 1, 2, 3, 5, 4, 6, 7, 8]);
        let late = run(
            FaultConfig {
                reorder: Some((10, 5)),
                ..Default::default()
            },
            20,
        );
        // packet 9 is held until 5 later ones went out
        let pos9 = late.iter().position(|&s| s == 9).unwrap();
        assert_eq!(pos9, 14);
        assert_eq!(late.len(), 19, "packet 19 is held at the end");
    }

    #[test]
    fn end_markers_pass() {
        let mut s = FaultState::new(FaultConfig {
            drop_every: Some(1),
            ..Default::default()
        });
        let mut end = pkt(3);
        end.payload.clear();
        assert_eq!(s.apply(end).len(), 1);
    }
}
