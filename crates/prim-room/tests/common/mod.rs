//! Shared harness: nodes backed by the in-memory or the real iroh transport, driven
//! through their local sockets exactly as `prim ctl` would.
#![allow(dead_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use prim_room::daemon::{Daemon, DaemonOpts, Start};
use prim_room::local::{b64, unb64, Client, Event, ReplyJson, Request, StatusJson, UpdateJson};
use prim_room::transport::iroh::{IrohConfig, IrohTransport, PeerAddrs, RelaySetting};
use prim_room::transport::mem::MemNetwork;
use room_core::api::Timing;
use room_core::ids::Key;

pub fn fast_timing() -> Timing {
    Timing {
        presence_interval_ms: 200,
        presence_expiry_ms: 600,
        coordinator_liveness_ms: 800,
        rejoin_grace_ms: 1_000,
        hello_timeout_ms: 2_000,
        redial_ms: 200,
    }
}

pub struct Node {
    pub daemon: Daemon,
    pub client: Client,
    pub state_dir: PathBuf,
    pub socket: PathBuf,
    /// The realtime voice socket, when started with `start_rt`.
    pub rt_socket: Option<PathBuf>,
}

impl Node {
    pub fn key(&self) -> Key {
        self.daemon.key
    }

    pub async fn status(&mut self) -> StatusJson {
        match self
            .client
            .call(|req| Request::Status { req })
            .await
            .unwrap()
        {
            ReplyJson::Status { status } => status,
            other => panic!("status: {other:?}"),
        }
    }

    pub async fn invite(&mut self) -> String {
        match self
            .client
            .call(|req| Request::CreateInvite {
                req,
                expiry_in_ms: 60_000,
                relays: vec![],
            })
            .await
            .unwrap()
        {
            ReplyJson::Invite { token } => token,
            other => panic!("invite: {other:?}"),
        }
    }

    pub async fn ok(&mut self, make: impl FnOnce(u64) -> Request) {
        match self.client.call(make).await.unwrap() {
            ReplyJson::Ok => {}
            other => panic!("expected ok, got {other:?}"),
        }
    }

    /// Wait for an event matching `pred`, skipping the others.
    pub async fn wait_event(&mut self, secs: u64, mut pred: impl FnMut(&Event) -> bool) -> Event {
        tokio::time::timeout(Duration::from_secs(secs), async {
            loop {
                match self.client.next_event().await.unwrap() {
                    Some(e) if pred(&e) => return e,
                    Some(_) => {}
                    None => panic!("socket closed while waiting"),
                }
            }
        })
        .await
        .expect("timed out waiting for an event")
    }
}

/// Poll a condition (an expression that may `.await`) until it is true or `secs` pass.
#[macro_export]
macro_rules! eventually {
    ($secs:expr, $what:expr, $cond:expr) => {{
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs($secs);
        loop {
            if $cond {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                panic!("timed out: {}", $what);
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }};
}

pub struct Fleet {
    pub dir: tempfile::TempDir,
    kind: Kind,
}

enum Kind {
    Mem(MemNetwork),
    Iroh(PeerAddrs),
}

impl Fleet {
    pub fn mem() -> Fleet {
        Fleet {
            dir: tempfile::tempdir().unwrap(),
            kind: Kind::Mem(MemNetwork::new()),
        }
    }

    pub fn iroh_loopback() -> Fleet {
        Fleet {
            dir: tempfile::tempdir().unwrap(),
            kind: Kind::Iroh(Arc::new(Mutex::new(HashMap::new()))),
        }
    }

    pub async fn start(&self, name: &str, start: Start) -> Node {
        self.start_opts(name, start, false).await
    }

    /// Like `start`, with the realtime voice lane on.
    pub async fn start_rt(&self, name: &str, start: Start) -> Node {
        self.start_opts(name, start, true).await
    }

    async fn start_opts(&self, name: &str, start: Start, rt: bool) -> Node {
        let root = self.dir.path().join(name);
        let state_dir = root.join("state");
        let socket = root.join("s.sock");
        std::fs::create_dir_all(&root).unwrap();
        let opts = DaemonOpts {
            state_dir: Some(state_dir.clone()),
            socket: Some(socket.clone()),
            timing: fast_timing(),
            persist_debounce: Duration::from_millis(50),
            rt_socket: rt.then(|| root.join("rt.sock")),
            ..Default::default()
        };
        let rt_socket = opts.rt_socket.clone();
        let daemon = match &self.kind {
            Kind::Mem(net) => {
                let net = net.clone();
                Daemon::start(
                    opts,
                    start,
                    move |id| async move { Ok(net.transport(id.key())) },
                )
                .await
                .unwrap()
            }
            Kind::Iroh(addrs) => {
                let addrs = addrs.clone();
                Daemon::start(opts, start, move |id| async move {
                    let cfg = IrohConfig {
                        relay: RelaySetting::Disabled,
                        bind: Some("127.0.0.1:0".parse::<SocketAddr>().unwrap()),
                        peer_addrs: addrs.clone(),
                        dial_timeout: Duration::from_secs(5),
                        ..Default::default()
                    };
                    let t = IrohTransport::bind(id.secret(), cfg)
                        .await
                        .map_err(|e| anyhow::anyhow!("{e}"))?;
                    addrs.lock().unwrap().insert(id.key(), t.bound_sockets());
                    Ok(t)
                })
                .await
                .unwrap()
            }
        };
        let client = Client::connect(&socket).await.unwrap();
        Node {
            daemon,
            client,
            state_dir,
            socket,
            rt_socket,
        }
    }
}

pub fn text_of(update: &UpdateJson) -> Option<String> {
    match update {
        UpdateJson::Value { value, .. } => {
            Some(String::from_utf8_lossy(&unb64(value).ok()?).into())
        }
        _ => None,
    }
}

pub fn payload(s: &str) -> String {
    b64(s.as_bytes())
}

pub fn path_exists(p: &Path) -> bool {
    p.exists()
}
