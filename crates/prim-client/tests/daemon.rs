//! PrimClient against real daemons (in-process, in-memory peer transport, real Unix
//! sockets for the local API): snapshot-then-deltas, live write round trip, message ack,
//! reconnect after a daemon restart.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use prim_client::transport::UnixTransport;
use prim_client::wire::{unb64, Event, ReplyJson, Request, UpdateJson};
use prim_client::{Client, ClientEvent};
use prim_room::daemon::{Daemon, DaemonOpts, Start};
use prim_room::transport::mem::MemNetwork;
use room_core::api::Timing;

fn fast_timing() -> Timing {
    Timing {
        presence_interval_ms: 200,
        presence_expiry_ms: 600,
        coordinator_liveness_ms: 800,
        rejoin_grace_ms: 1_000,
        hello_timeout_ms: 2_000,
        redial_ms: 200,
    }
}

struct Node {
    daemon: Option<Daemon>,
    socket: PathBuf,
    state_dir: PathBuf,
}

struct World {
    rt: tokio::runtime::Runtime,
    dir: tempfile::TempDir,
    net: MemNetwork,
}

impl World {
    fn new() -> World {
        World {
            rt: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .enable_all()
                .build()
                .unwrap(),
            dir: tempfile::tempdir().unwrap(),
            net: MemNetwork::new(),
        }
    }

    fn start(&self, name: &str, start: Start) -> Node {
        let root = self.dir.path().join(name);
        std::fs::create_dir_all(&root).unwrap();
        let state_dir = root.join("state");
        let socket = root.join("s.sock");
        let daemon = self.launch(&state_dir, &socket, start);
        Node {
            daemon: Some(daemon),
            socket,
            state_dir,
        }
    }

    fn launch(&self, state_dir: &PathBuf, socket: &PathBuf, start: Start) -> Daemon {
        let opts = DaemonOpts {
            state_dir: Some(state_dir.clone()),
            socket: Some(socket.clone()),
            timing: fast_timing(),
            persist_debounce: Duration::from_millis(50),
            ..Default::default()
        };
        let net = self.net.clone();
        self.rt.block_on(async move {
            Daemon::start(opts, start, move |id| async move {
                Ok(net.transport(id.key()))
            })
            .await
            .unwrap()
        })
    }

    fn stop(&self, n: &mut Node) {
        let d = n.daemon.take().unwrap();
        self.rt.block_on(d.shutdown());
    }

    fn restart(&self, n: &mut Node) {
        n.daemon = Some(self.launch(&n.state_dir, &n.socket, Start::Resume));
    }
}

fn client(n: &Node) -> Client {
    Client::start(UnixTransport::new(&n.socket))
}

/// Pump events until `pred` returns Some, or fail after `secs`.
fn wait<T>(c: &Client, secs: u64, what: &str, mut pred: impl FnMut(&ClientEvent) -> Option<T>) -> T {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if let Some(e) = c.recv_timeout(Duration::from_millis(50)) {
            if let Some(t) = pred(&e) {
                return t;
            }
        }
    }
    panic!("timed out waiting for {what}");
}

fn eventually(secs: u64, what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out: {what}");
}

fn call_ok(c: &Client, make: impl FnOnce(u64) -> Request) {
    match c.call(make, Duration::from_secs(5)).unwrap() {
        ReplyJson::Ok => {}
        other => panic!("expected ok, got {other:?}"),
    }
}

fn invite(c: &Client) -> String {
    match c
        .call(
            |req| Request::CreateInvite {
                req,
                expiry_in_ms: 60_000,
                relays: vec![],
            },
            Duration::from_secs(5),
        )
        .unwrap()
    {
        ReplyJson::Invite { token } => token,
        other => panic!("{other:?}"),
    }
}

fn members(c: &Client) -> usize {
    match c.call(|req| Request::Status { req }, Duration::from_secs(5)) {
        Ok(ReplyJson::Status { status }) => status["members"].as_array().map_or(0, |m| m.len()),
        _ => 0,
    }
}

/// Values of every `Value` update for `sub` seen so far in `evs`.
fn values(evs: &[ClientEvent], sub: u32) -> Vec<(String, Vec<u8>)> {
    let mut out = vec![];
    for e in evs {
        if let ClientEvent::Event(Event::Updates { sub: s, updates }) = e {
            if *s == sub {
                for u in updates {
                    if let UpdateJson::Value { path, value, .. } = u {
                        out.push((path.clone(), unb64(value).unwrap()));
                    }
                }
            }
        }
    }
    out
}

#[test]
fn subscribe_gives_snapshot_then_deltas() {
    let w = World::new();
    let a = w.start("a", Start::Found { meta: vec![] });
    let c = client(&a);
    wait(&c, 5, "connect", |e| matches!(e, ClientEvent::Connected).then_some(()));
    // Durable write first, subscribe after: it must come back in the snapshot.
    eventually(5, "coordinator", || {
        c.write("member", "note", false, false, b"one").is_ok_and(|req| {
            // Accept only once the daemon says ok.
            matches!(
                wait_reply(&c, req),
                Some(ReplyJson::Ok)
            )
        })
    });
    let sub = c.subscribe("member/*/note").unwrap();
    let mut evs = vec![];
    wait(&c, 5, "snapshot", |e| {
        evs.push(e.clone());
        matches!(e, ClientEvent::Event(Event::Updates { updates, .. })
            if updates.iter().any(|u| matches!(u, UpdateJson::SnapshotDone)))
        .then_some(())
    });
    let snap = values(&evs, sub);
    assert_eq!(snap.len(), 1, "{evs:?}");
    assert_eq!(snap[0].1, b"one");
    // A later write arrives as a delta after SnapshotDone.
    let req = c.write("member", "note", false, false, b"two").unwrap();
    let _ = req;
    let mut evs = vec![];
    wait(&c, 5, "delta", |e| {
        evs.push(e.clone());
        values(&evs, sub).iter().any(|(_, v)| v == b"two").then_some(())
    });
}

/// Wait for the Reply event of `req`.
fn wait_reply(c: &Client, req: u64) -> Option<ReplyJson> {
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        if let Some(ClientEvent::Event(Event::Reply { req: r, reply })) =
            c.recv_timeout(Duration::from_millis(50))
        {
            if r == req {
                return Some(reply);
            }
        }
    }
    None
}

fn two_nodes(w: &World) -> (Node, Node, Client, Client) {
    let a = w.start("a", Start::Found { meta: vec![] });
    let ca = client(&a);
    wait(&ca, 5, "a connect", |e| matches!(e, ClientEvent::Connected).then_some(()));
    let token = invite(&ca);
    let b = w.start("b", Start::Join(token));
    let cb = client(&b);
    wait(&cb, 5, "b connect", |e| matches!(e, ClientEvent::Connected).then_some(()));
    eventually(10, "both see 2 members", || members(&ca) == 2 && members(&cb) == 2);
    (a, b, ca, cb)
}

#[test]
fn live_write_round_trips_between_peers() {
    let w = World::new();
    let (_a, _b, ca, cb) = two_nodes(&w);
    let sub = ca.subscribe("member/*/pose").unwrap();
    // b writes a live pose; a must see it with b's key as writer.
    let end = Instant::now() + Duration::from_secs(10);
    let mut seen = None;
    let mut n = 0u32;
    while seen.is_none() && Instant::now() < end {
        n += 1;
        let _ = cb.write("member", "pose", true, false, format!("p{n}").as_bytes());
        if let Some(ClientEvent::Event(Event::Updates { sub: s, updates })) =
            ca.recv_timeout(Duration::from_millis(100))
        {
            if s == sub {
                for u in updates {
                    if let UpdateJson::Value { path, class, value, .. } = u {
                        if class == "live" && path.ends_with("/pose") {
                            seen = Some((path, unb64(&value).unwrap()));
                        }
                    }
                }
            }
        }
    }
    let (path, value) = seen.expect("live pose never arrived");
    assert!(path.starts_with("member/"), "{path}");
    assert!(String::from_utf8(value).unwrap().starts_with('p'));
    drop(w);
}

#[test]
fn message_is_delivered_and_acked_and_unregistered_channel_reports_no_channel() {
    let w = World::new();
    let (_a, _b, ca, cb) = two_nodes(&w);
    let b_key = match cb.call(|req| Request::Status { req }, Duration::from_secs(5)).unwrap() {
        ReplyJson::Status { status } => status["me"].as_str().unwrap().to_string(),
        o => panic!("{o:?}"),
    };
    call_ok(&ca, |req| Request::RegisterChannel { req, channel: 7 });
    // B -> A on channel 7.
    let req = cb.send("coordinator", 7, true, b"hello").unwrap_or_else(|_| unreachable!());
    let _ = req;
    // Whoever is coordinator, address A directly instead for determinism.
    let a_key = match ca.call(|req| Request::Status { req }, Duration::from_secs(5)).unwrap() {
        ReplyJson::Status { status } => status["me"].as_str().unwrap().to_string(),
        o => panic!("{o:?}"),
    };
    assert_ne!(a_key, b_key);
    cb.send(&a_key, 7, true, b"direct").unwrap();
    let got = wait(&ca, 5, "message at a", |e| match e {
        ClientEvent::Event(Event::Message { from, channel, payload }) if *channel == 7 => {
            Some((from.clone(), unb64(payload).unwrap()))
        }
        _ => None,
    });
    assert_eq!(got.0, b_key);
    assert!(got.1 == b"hello" || got.1 == b"direct");
    let status = wait(&cb, 5, "ack at b", |e| match e {
        ClientEvent::Event(Event::MessageResult { status, .. }) if status == "delivered" => {
            Some(status.clone())
        }
        _ => None,
    });
    assert_eq!(status, "delivered");
    // Channel 9 is registered nowhere.
    cb.send(&a_key, 9, true, b"x").unwrap();
    wait(&cb, 5, "no_channel", |e| match e {
        ClientEvent::Event(Event::MessageResult { status, .. }) if status == "no_channel" => Some(()),
        _ => None,
    });
}

#[test]
fn reconnects_after_daemon_restart_and_replays_subscription_and_channel() {
    let w = World::new();
    let mut a = w.start("a", Start::Found { meta: vec![] });
    let c = client(&a);
    wait(&c, 5, "connect", |e| matches!(e, ClientEvent::Connected).then_some(()));
    let sub = c.subscribe("member/*/note").unwrap();
    let rc = c.register_channel(3).unwrap();
    assert_eq!(wait_reply(&c, rc), Some(ReplyJson::Ok));
    eventually(5, "durable note accepted", || {
        c.write("member", "note", false, true, b"kept")
            .map(|r| wait_reply(&c, r) == Some(ReplyJson::Ok))
            .unwrap_or(false)
    });
    let me0 = match c.call(|req| Request::Status { req }, Duration::from_secs(5)).unwrap() {
        ReplyJson::Status { status } => status["me"].as_str().unwrap().to_string(),
        o => panic!("{o:?}"),
    };
    c.send(&me0, 3, true, b"pre").unwrap();
    let st = wait(&c, 5, "pre-restart self result", |e| match e {
        ClientEvent::Event(Event::MessageResult { status, .. }) => Some(status.clone()),
        _ => None,
    });
    assert_eq!(st, "delivered", "self-send works before restart");
    w.stop(&mut a);
    wait(&c, 5, "disconnect", |e| matches!(e, ClientEvent::Disconnected).then_some(()));
    assert!(!c.is_connected());
    assert!(c.write("member", "note", false, true, b"x").is_err(), "writes fail while down");
    w.restart(&mut a);
    wait(&c, 10, "reconnect", |e| matches!(e, ClientEvent::Connected).then_some(()));
    // Same sub id, fresh snapshot containing the persistent record.
    let mut evs = vec![];
    wait(&c, 10, "snapshot after reconnect", |e| {
        evs.push(e.clone());
        values(&evs, sub).iter().any(|(_, v)| v == b"kept").then_some(())
    });
    // Channel registration was replayed: a self-send on channel 3 is delivered, not no_channel.
    let me = match c.call(|req| Request::Status { req }, Duration::from_secs(5)).unwrap() {
        ReplyJson::Status { status } => status["me"].as_str().unwrap().to_string(),
        o => panic!("{o:?}"),
    };
    eventually(10, "room open again", || {
        matches!(c.call(|req| Request::Status { req }, Duration::from_secs(5)),
            Ok(ReplyJson::Status { status }) if status["lifecycle"] == "open")
    });
    c.send(&me, 3, true, b"self").unwrap();
    let st = wait(&c, 5, "self message result", |e| match e {
        ClientEvent::Event(Event::Reply { reply: ReplyJson::Error { code }, .. }) => panic!("replay failed: {code}"),
        ClientEvent::Event(Event::MessageResult { status, .. }) => Some(status.clone()),
        _ => None,
    });
    assert_eq!(st, "delivered", "channel 3 registration must have been replayed");
    call_ok(&c, |req| Request::Leave { req });
}
