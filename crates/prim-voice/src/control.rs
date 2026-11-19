//! Runtime control of a running voice client: mic mute, deafen, and what to do when the
//! tracking lease goes away. The daemon forwards `VoiceControl`; this client owns the truth
//! and reports it back (`SetState` for peers, `VoiceReport` for the requester).
//!
//! Threads never wait on each other for this: the reader hands a command to the control
//! thread, which sets the atomics, waits (bounded) until the encoder and the mixer have
//! passed a frame boundary with the new targets, then reports.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;

use room_core::rt::{report_bits, RtMsg};
use room_core::voice::voice_state;

use crate::clock::mono_us;
use crate::config::{CaptureSpec, Config};
use crate::shared::Shared;

/// Longest the control thread waits for the encoder and the mixer to honour a change.
pub const APPLY_WAIT_US: u64 = 100_000;

pub struct Control {
    /// Target: the microphone is live (not muted).
    pub mic_live: AtomicBool,
    pub deaf: AtomicBool,
    /// The user's tray (the tracking process) is the mute authority: do not mute on its loss.
    pub tray_live: AtomicBool,
    pub tracking_held: AtomicBool,
    /// Bumped after every change of the targets.
    pub version: AtomicU64,
    /// The newest version the encoder / the mixer has run a pass under.
    pub enc_seen: AtomicU64,
    pub mix_seen: AtomicU64,
}

impl Control {
    pub fn new(cfg: &Config) -> Control {
        Control {
            mic_live: AtomicBool::new(!cfg.start_muted),
            deaf: AtomicBool::new(cfg.start_deaf),
            tray_live: AtomicBool::new(cfg.tray_live),
            tracking_held: AtomicBool::new(false),
            version: AtomicU64::new(0),
            enc_seen: AtomicU64::new(0),
            mix_seen: AtomicU64::new(0),
        }
    }

    /// `report_bits` for the current targets.
    pub fn report_state(&self) -> u8 {
        let mut s = 0;
        if !self.mic_live.load(Ordering::SeqCst) {
            s |= report_bits::MUTED;
        }
        if self.deaf.load(Ordering::SeqCst) {
            s |= report_bits::DEAF;
        }
        if self.tray_live.load(Ordering::SeqCst) {
            s |= report_bits::TRAY_LIVE;
        }
        s
    }

    /// The `member/<id>/voice` flags peers see.
    pub fn wire_flags(&self, cfg: &Config) -> u8 {
        wire_flags(
            self.mic_live.load(Ordering::SeqCst),
            self.deaf.load(Ordering::SeqCst),
            !matches!(cfg.capture, CaptureSpec::None),
        )
    }
}

pub fn wire_flags(mic_live: bool, deaf: bool, has_capture: bool) -> u8 {
    let mut f = 0;
    if !deaf {
        f |= voice_state::LISTENING;
    }
    if mic_live && has_capture {
        f |= voice_state::MIC;
    }
    f
}

/// What the control thread is asked to do.
#[derive(Debug, Clone, Copy)]
pub enum Cmd {
    /// Apply the given targets; `req` is the daemon's request id (0: our own decision).
    Apply {
        req: u32,
        mic: Option<bool>,
        deaf: Option<bool>,
        tray_live: Option<bool>,
    },
    /// Re-announce the current state (after a (re)connect).
    Announce,
}

pub fn spawn(sh: Arc<Shared>, rx: Receiver<Cmd>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("voice-control".into())
        .spawn(move || {
            while !sh.is_shutdown() {
                match rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(Cmd::Apply {
                        req,
                        mic,
                        deaf,
                        tray_live,
                    }) => apply(&sh, req, mic, deaf, tray_live),
                    Ok(Cmd::Announce) => report(&sh, 0),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        })
        .expect("spawn control")
}

fn apply(sh: &Shared, req: u32, mic: Option<bool>, deaf: Option<bool>, tray_live: Option<bool>) {
    let c = &sh.ctl;
    let before = (
        c.mic_live.load(Ordering::SeqCst),
        c.deaf.load(Ordering::SeqCst),
    );
    if let Some(v) = tray_live {
        c.tray_live.store(v, Ordering::SeqCst);
    }
    if let Some(v) = mic {
        c.mic_live.store(v, Ordering::SeqCst);
    }
    if let Some(v) = deaf {
        c.deaf.store(v, Ordering::SeqCst);
    }
    let now = (
        c.mic_live.load(Ordering::SeqCst),
        c.deaf.load(Ordering::SeqCst),
    );
    if before.0 != now.0 {
        sh.stats.bump(if now.0 { "mic_unmutes" } else { "mic_mutes" });
    }
    if before.1 != now.1 {
        sh.stats.bump(if now.1 { "deafens" } else { "undeafens" });
    }
    let v = c.version.fetch_add(1, Ordering::SeqCst) + 1;
    let deadline = mono_us() + APPLY_WAIT_US;
    while (c.enc_seen.load(Ordering::SeqCst) < v || c.mix_seen.load(Ordering::SeqCst) < v)
        && mono_us() < deadline
        && !sh.is_shutdown()
    {
        std::thread::sleep(Duration::from_millis(1));
    }
    if c.enc_seen.load(Ordering::SeqCst) < v || c.mix_seen.load(Ordering::SeqCst) < v {
        sh.stats.bump("control_ack_timeouts");
    }
    sh.stats.bump("control_applies");
    report(sh, req);
}

/// Tell the daemon (and so peers) the state: the wire flags first, then the report that
/// answers `ack`.
fn report(sh: &Shared, ack: u32) {
    let flags = sh.ctl.wire_flags(&sh.cfg);
    let state = sh.ctl.report_state();
    // A report during a reconnect is lost; the reader announces again once it is up.
    if !sh.link.is_up() {
        sh.stats.bump("control_report_no_link");
        return;
    }
    sh.link.send(&RtMsg::SetState {
        flags,
        incarnation: sh.incarnation,
    });
    sh.link.send(&RtMsg::VoiceReport { ack, state });
    sh.last_send_us.store(mono_us(), Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_flags_follow_the_targets() {
        assert_eq!(wire_flags(true, false, true), voice_state::LISTENING | voice_state::MIC);
        assert_eq!(wire_flags(false, false, true), voice_state::LISTENING);
        assert_eq!(wire_flags(true, true, true), voice_state::MIC);
        assert_eq!(wire_flags(false, true, true), 0);
        assert_eq!(wire_flags(true, false, false), voice_state::LISTENING, "no capture: never MIC");
    }

    #[test]
    fn start_flags_set_the_targets() {
        let cfg = Config {
            start_muted: true,
            start_deaf: true,
            tray_live: true,
            ..Config::default()
        };
        let c = Control::new(&cfg);
        assert_eq!(
            c.report_state(),
            report_bits::MUTED | report_bits::DEAF | report_bits::TRAY_LIVE
        );
        assert_eq!(Control::new(&Config::default()).report_state(), 0);
    }
}
