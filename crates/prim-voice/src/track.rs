//! `prim voice track`: connect as the tracking source and send head poses at a fixed
//! rate with the yaw sweeping, so the listener-age oracle has a source to measure.

use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Result;
use room_core::rt::{Pose, Role, RtMsg};

use crate::clock::{mono_us, sleep_until};
use crate::rtio::{handshake, RtConn, SendOutcome};
use crate::signals;

pub struct TrackOpts {
    pub rate_hz: f64,
    pub sweep_deg_per_s: f64,
    pub takeover: bool,
    pub duration: Option<Duration>,
}

/// Head at 1.6 m, yaw about +Y.
pub fn pose_at(yaw_deg: f64) -> Pose {
    let half = (yaw_deg.to_radians() / 2.0) as f32;
    Pose {
        pos: [0.0, 1.6, 0.0],
        rot: [0.0, half.sin(), 0.0, half.cos()],
    }
}

/// Returns the number of poses sent.
pub fn run_tracker(conn: &RtConn, o: &TrackOpts) -> Result<u64> {
    handshake(conn, Role::TrackingSource, o.takeover, 0, 0)?;
    let interval_us = (1e6 / o.rate_hz) as u64;
    let t0 = mono_us();
    let mut seq: u32 = 0;
    let mut next = t0;
    loop {
        if signals::TERM.load(Ordering::SeqCst) {
            break;
        }
        let now = mono_us();
        if let Some(d) = o.duration {
            if now - t0 >= d.as_micros() as u64 {
                break;
            }
        }
        sleep_until(next);
        let t = mono_us();
        let yaw = o.sweep_deg_per_s * (t - t0) as f64 * 1e-6;
        let sent = conn.send(&RtMsg::PoseOut {
            source_mono_us: t,
            seq,
            pose: pose_at(yaw),
        });
        if sent == SendOutcome::Closed {
            anyhow::bail!("realtime socket closed");
        }
        seq = seq.wrapping_add(1);
        next += interval_us;
        // Drain whatever the daemon sent (a Revoked ends us).
        while let Ok(Some(b)) = conn.recv(Duration::ZERO) {
            if let Ok(RtMsg::Revoked { reason }) = RtMsg::decode(&b) {
                anyhow::bail!("tracking lease revoked (reason {reason})");
            }
        }
    }
    Ok(seq as u64)
}
