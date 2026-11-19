//! The drift-correction decision, ported from `project/media/playback.gd:436-466`
//! (`correct`) and `playback.gd:49-50` (`target_position`).
//!
//! Pure: inputs are the room target, the player's reported position and the flags the
//! old code kept; the output is what to tell the player. The old constants are kept:
//! seek over 0.75 s, a forced (new revision, or recovery from buffering) seek over
//! 0.12 s, speed `1 + drift * 0.08` clamped to 0.98..1.02 when the drift exceeds 0.04 s
//! and the room is playing.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    pub seek_threshold_s: f64,
    pub force_threshold_s: f64,
    pub nudge_deadband_s: f64,
    pub nudge_gain: f64,
    pub nudge_min: f64,
    pub nudge_max: f64,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            seek_threshold_s: 0.75,
            force_threshold_s: 0.12,
            nudge_deadband_s: 0.04,
            nudge_gain: 0.08,
            nudge_min: 0.98,
            nudge_max: 1.02,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Inputs {
    /// Where the room is now, already clamped to the duration (GDScript `target`).
    pub target_s: f64,
    /// The player's reported position.
    pub position_s: f64,
    /// The room's rate (1.0 in the old code, which had no rate).
    pub rate: f64,
    pub desired_paused: bool,
    pub force: bool,
    pub awaiting_seek: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Decision {
    pub drift_s: f64,
    /// `Some(target)` = seek there (and reset the speed to the room rate).
    pub seek_to_s: Option<f64>,
    pub speed: f64,
    pub paused: bool,
}

/// `max(0, position + (host_now - stamp) * rate)` while playing; `position` while paused.
pub fn target_position(
    position_s: f64,
    stamp_s: f64,
    host_now_s: f64,
    paused: bool,
    rate: f64,
) -> f64 {
    let elapsed = if paused {
        0.0
    } else {
        (host_now_s - stamp_s).max(0.0) * rate
    };
    (position_s + elapsed).max(0.0)
}

pub fn correct(p: &Params, i: &Inputs) -> Decision {
    let drift = i.target_s - i.position_s;
    let seek = i.awaiting_seek
        || (i.force && drift.abs() > p.force_threshold_s)
        || drift.abs() > p.seek_threshold_s;
    if seek {
        Decision {
            drift_s: drift,
            seek_to_s: Some(i.target_s),
            speed: i.rate,
            paused: i.desired_paused,
        }
    } else {
        let nudge = if drift.abs() > p.nudge_deadband_s && !i.desired_paused {
            (1.0 + drift * p.nudge_gain).clamp(p.nudge_min, p.nudge_max)
        } else {
            1.0
        };
        Decision {
            drift_s: drift,
            seek_to_s: None,
            speed: i.rate * nudge,
            paused: i.desired_paused,
        }
    }
}
