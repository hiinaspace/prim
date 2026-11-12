// SPDX-License-Identifier: Unlicense
//! `prim-voice`: the voice client of a `prim-room` daemon (docs/room/VOICE.md).
//!
//! It holds the `voice` lease on the daemon's realtime socket, sends 20 ms Opus
//! packets from a capture source, and renders everyone else's voice through NetEq and
//! prim-hrtf onto an output. `prim voice` is a thin wrapper over [`run`].

pub mod capture;
pub mod client;
pub mod clock;
pub mod config;
pub mod device;
pub mod hist;
pub mod mixer;
pub mod output;
pub mod recv;
pub mod ring;
pub mod rtio;
pub mod seats;
pub mod send;
pub mod shared;
pub mod signals;
pub mod stats;
pub mod tone;
pub mod track;

pub use client::Client;
pub use config::{ArrivalPolicy, CaptureSpec, Config, Flags, OutputSpec, HELP, VALUED_FLAGS};
pub use rtio::{HandshakeError, RtConn};
pub use shared::ExitReason;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::{Context, Result};

/// Exit codes: 0 normal, 1 failure, 2 usage, 3 lease refused (busy), 4 lease revoked,
/// 5 the daemon went away.
pub fn run(f: &dyn Flags) -> Result<ExitCode> {
    if f.has("help") {
        eprint!("{HELP}");
        return Ok(ExitCode::from(2));
    }
    let socket = f
        .value("rt-socket")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("PRIM_RT_SOCKET").map(PathBuf::from))
        .unwrap_or_else(RtConn::default_path);
    let duration = match f.value("duration") {
        Some(v) => Some(Duration::from_secs_f64(
            v.parse().with_context(|| format!("--duration {v:?}"))?,
        )),
        None => None,
    };
    signals::install();
    let sub = f.positional().first().map(String::as_str);
    match sub {
        Some("track") => {
            let rate: f64 = f
                .value("rate")
                .map(str::parse)
                .transpose()
                .context("--rate")?
                .unwrap_or(90.0);
            let sweep: f64 = f
                .value("sweep")
                .map(str::parse)
                .transpose()
                .context("--sweep")?
                .unwrap_or(30.0);
            let conn = RtConn::connect(&socket)
                .with_context(|| format!("connecting to {}", socket.display()))?;
            let sent = track::run_tracker(
                &conn,
                &track::TrackOpts {
                    rate_hz: rate,
                    sweep_deg_per_s: sweep,
                    takeover: f.has("takeover"),
                    duration,
                },
            )?;
            eprintln!("prim voice track: sent {sent} poses");
            Ok(ExitCode::SUCCESS)
        }
        None => run_client(f, &socket, duration),
        Some(other) => {
            eprintln!("prim voice: unknown subcommand {other:?}\n{HELP}");
            Ok(ExitCode::from(2))
        }
    }
}

fn run_client(
    f: &dyn Flags,
    socket: &std::path::Path,
    duration: Option<Duration>,
) -> Result<ExitCode> {
    let cfg = Config::from_flags(f)?;
    let conn =
        RtConn::connect(socket).with_context(|| format!("connecting to {}", socket.display()))?;
    let client = match Client::start(conn, cfg) {
        Ok(c) => c,
        Err(e) => {
            if let Some(h) = e.chain().find_map(|c| c.downcast_ref::<HandshakeError>()) {
                eprintln!("prim voice: {h}");
                return Ok(ExitCode::from(match h {
                    HandshakeError::Refused(_) => 3,
                    HandshakeError::Revoked(_) => 4,
                    _ => 1,
                }));
            }
            return Err(e);
        }
    };
    eprintln!(
        "prim voice: lease held as {} (incarnation {})",
        client.self_key().short(),
        client.incarnation()
    );
    let t0 = std::time::Instant::now();
    let reason = loop {
        if signals::TERM.load(Ordering::SeqCst) {
            break ExitReason::Stopped;
        }
        if signals::USR1.swap(false, Ordering::SeqCst) {
            client.request_device_change();
        }
        if let Some(r) = client.exit_reason() {
            break r;
        }
        if duration.is_some_and(|d| t0.elapsed() >= d) {
            break ExitReason::Stopped;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    client.stop();
    Ok(match reason {
        ExitReason::Stopped => ExitCode::SUCCESS,
        ExitReason::Revoked(r) => {
            eprintln!("prim voice: lease revoked (reason {r})");
            ExitCode::from(4)
        }
        ExitReason::SocketClosed => {
            eprintln!("prim voice: the daemon closed the realtime socket");
            ExitCode::from(5)
        }
    })
}
