//! `prim room found|join|resume`: run the daemon.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use prim_room::daemon::{Daemon, DaemonOpts, Start};
use prim_room::local;
use prim_room::transport::iroh::{IrohConfig, IrohTransport, RelaySetting};
use room_core::ids::{Key, RoomId};

use crate::Args;

const HELP: &str = "\
prim room found [--meta TEXT] [options]
prim room join <token> [options]
prim room resume <room-hex> [options]

options:
  --state-dir DIR        state directory (default $XDG_STATE_HOME/prim/rooms/<room-hex>)
  --socket PATH          local API socket (default $XDG_RUNTIME_DIR/prim/room.sock)
  --relay n0|none|URL    relay mode; repeat URL for several relays (default n0)
  --bind IP:PORT         bind one socket instead of the defaults
  --peer-addr KEY=IP:PORT  extra direct address for a member (repeatable)
  --dns IP[:PORT]        nameserver to use instead of the system's (also env PRIM_DNS)
";

pub async fn run(args: Vec<String>) -> Result<ExitCode> {
    init_tracing();
    let args = Args::parse(
        args,
        &[
            "state-dir",
            "socket",
            "relay",
            "bind",
            "peer-addr",
            "meta",
            "dns",
        ],
    )?;
    if args.has("help") || args.positional.is_empty() {
        eprint!("{HELP}");
        return Ok(ExitCode::from(2));
    }
    let mut opts = DaemonOpts {
        state_dir: args.value("state-dir").map(PathBuf::from),
        socket: Some(
            args.value("socket")
                .map(PathBuf::from)
                .unwrap_or_else(local::default_socket_path),
        ),
        ..Default::default()
    };
    let start = match args.positional[0].as_str() {
        "found" => Start::Found {
            meta: args.value("meta").unwrap_or("").as_bytes().to_vec(),
        },
        "join" => Start::Join(
            args.positional
                .get(1)
                .context("join needs an invite token")?
                .clone(),
        ),
        "resume" => {
            if opts.state_dir.is_none() {
                let hex = args
                    .positional
                    .get(1)
                    .context("resume needs a room id (hex)")?;
                opts.resume_room = Some(RoomId::from_hex(hex).context("bad room id")?);
            }
            Start::Resume
        }
        other => bail!("unknown mode {other:?} (found, join or resume)"),
    };

    let mut cfg = IrohConfig::default();
    let relays = args.values("relay");
    cfg.relay = match relays.as_slice() {
        [] | ["n0"] => RelaySetting::N0,
        ["none"] => RelaySetting::Disabled,
        urls => RelaySetting::Custom(urls.iter().map(|s| s.to_string()).collect()),
    };
    if let Some(b) = args.value("bind") {
        cfg.bind = Some(b.parse::<SocketAddr>().context("--bind")?);
    }
    let dns = args
        .value("dns")
        .map(str::to_string)
        .or_else(|| std::env::var("PRIM_DNS").ok().filter(|v| !v.is_empty()));
    if let Some(d) = dns {
        cfg.dns_server = Some(match d.parse::<SocketAddr>() {
            Ok(a) => a,
            Err(_) => SocketAddr::new(d.parse().context("--dns")?, 53),
        });
    }
    let mut peer_addrs: HashMap<Key, Vec<SocketAddr>> = HashMap::new();
    for pa in args.values("peer-addr") {
        let (k, a) = pa
            .split_once('=')
            .context("--peer-addr wants KEY=IP:PORT")?;
        peer_addrs
            .entry(Key::from_hex(k).context("--peer-addr key")?)
            .or_default()
            .push(a.parse().context("--peer-addr address")?);
    }
    *cfg.peer_addrs.lock().unwrap() = peer_addrs;

    let listen: std::sync::Arc<std::sync::Mutex<Vec<SocketAddr>>> = Default::default();
    let listen2 = listen.clone();
    let mut daemon = Daemon::start(opts, start, move |identity| async move {
        let t = IrohTransport::bind(identity.secret(), cfg)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        *listen2.lock().unwrap() = t.bound_sockets();
        Ok(t)
    })
    .await?;
    println!("member {}", daemon.key);
    println!("room {}", daemon.room);
    println!("incarnation {}", daemon.incarnation);
    println!("state-dir {}", daemon.state_dir.display());
    if let Some(s) = &daemon.socket {
        println!("socket {}", s.display());
    }
    for a in listen.lock().unwrap().iter() {
        println!("listen {a}");
    }
    println!("ready");
    use std::io::Write;
    let _ = std::io::stdout().flush();

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
        _ = daemon.wait() => {}
    }
    daemon.shutdown().await;
    Ok(ExitCode::SUCCESS)
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}
