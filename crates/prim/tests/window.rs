//! Round GW oracle: the client layer the room window uses, against real daemons over
//! loopback iroh. No window is opened; `prim_ui::client` is the same code the window
//! reads every frame. Voice uses the private test sinks (a tone "microphone", a null
//! output): never real speakers or a real microphone.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use prim_room::daemon::{Daemon, DaemonOpts, Start};
use prim_room::local::{Client, ReplyJson, Request};
use prim_room::transport::iroh::{IrohConfig, IrohTransport, PeerAddrs, RelaySetting};
use prim_ui::chat::Clients;
use prim_ui::client::{attach, send_chat_once, AttachOpts, Cmd, Handle, Presence};
use prim_ui::supervise::{self, ProcState, Voice};
use room_core::api::Timing;

struct Node {
    daemon: Daemon,
    socket: PathBuf,
    win: Handle,
}

struct Fleet {
    dir: tempfile::TempDir,
    addrs: PeerAddrs,
}

impl Fleet {
    fn new() -> Fleet {
        // The window spawns the real `prim` binary for voice.
        std::env::set_var("PRIM_UI_EXE", env!("CARGO_BIN_EXE_prim"));
        std::env::set_var("PRIM_UI_VOICE_TEST", "1");
        Fleet {
            dir: tempfile::tempdir().unwrap(),
            addrs: Arc::new(Mutex::new(Default::default())),
        }
    }

    async fn start(&self, name: &str, start: Start) -> Node {
        let root = self.dir.path().join(name);
        std::fs::create_dir_all(&root).unwrap();
        let socket = root.join("room.sock");
        let opts = DaemonOpts {
            state_dir: Some(root.join("state")),
            socket: Some(socket.clone()),
            rt_socket: Some(root.join("rt.sock")),
            timing: Timing {
                presence_interval_ms: 200,
                presence_expiry_ms: 800,
                coordinator_liveness_ms: 1_000,
                rejoin_grace_ms: 1_000,
                hello_timeout_ms: 2_000,
                redial_ms: 200,
            },
            persist_debounce: Duration::from_millis(50),
            ..Default::default()
        };
        let addrs = self.addrs.clone();
        let daemon = Daemon::start(opts, start, move |id| async move {
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
        .unwrap();
        let win = attach(
            &socket,
            AttachOpts {
                chat: true,
                default_name: Some(name.to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        Node {
            daemon,
            socket,
            win,
        }
    }

    async fn found(&self, name: &str) -> Node {
        self.start(name, Start::Found { meta: name.as_bytes().to_vec() })
            .await
    }

    /// Mint an invite through the window layer and join with it.
    async fn join(&self, name: &str, via: &Node) -> Node {
        let before = via.win.lock().invite.as_ref().map(|i| i.0).unwrap_or(0);
        via.win.send(Cmd::Invite);
        via.win
            .wait_for(5, "invite", |m| m.invite.as_ref().is_some_and(|i| i.0 > before))
            .await
            .unwrap();
        let token = via.win.lock().invite.clone().unwrap().1;
        self.start(name, Start::Join(token)).await
    }
}

fn online_names(h: &Handle) -> Vec<String> {
    let mut v: Vec<String> = h
        .lock()
        .members()
        .iter()
        .filter(|m| m.presence == Presence::Online)
        .map(|m| m.name.clone())
        .collect();
    v.sort();
    v
}

fn lines(h: &Handle) -> Vec<(String, String)> {
    let m = h.lock();
    m.chat
        .iter()
        .filter_map(|l| Some((m.name_of(l.from.as_ref()?), l.text.clone())))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn roster_chat_voice_and_non_members() {
    let f = Fleet::new();
    let a = f.found("alice").await;
    // Founding gives the window a room with only you in it.
    a.win.wait_for(5, "alice alone", |m| m.in_room()).await.unwrap();
    a.win
        .wait_for(5, "alice named", |m| m.members().iter().any(|r| r.is_me && r.name == "alice"))
        .await
        .unwrap();
    assert_eq!(online_names(&a.win), ["alice"]);

    // Create + invite + join: both rosters converge, with names and a route.
    let b = f.join("bob", &a).await;
    for (who, h) in [("alice", &a.win), ("bob", &b.win)] {
        h.wait_until(15, &format!("{who} sees both"), || online_names(h) == ["alice", "bob"])
            .await
            .unwrap();
    }
    a.win
        .wait_for(10, "route to bob", |m| {
            m.members().iter().any(|r| r.name == "bob" && r.route.as_ref().is_some_and(|rt| rt.path != ""))
        })
        .await
        .unwrap();
    assert!(a.win.lock().members().iter().any(|r| r.is_me && r.keyholder), "opener is a keyholder");
    assert!(b.win.lock().members().iter().all(|r| !r.keyholder || r.name == "alice"));

    // Chat both ways, with delivery results.
    a.win.chat("test");
    b.win.wait_until(5, "bob gets test", || lines(&b.win).contains(&("alice".into(), "test".into()))).await.unwrap();
    b.win.chat("something's not working");
    a.win
        .wait_until(5, "alice gets reply", || lines(&a.win).contains(&("bob".into(), "something's not working".into())))
        .await
        .unwrap();
    a.win
        .wait_for(5, "delivery receipt", |m| {
            m.chat.iter().any(|l| {
                l.text == "test"
                    && matches!(&l.delivery, Some(prim_ui::client::Delivery::Done { ok: 1, total: 1, failed }) if failed.is_empty())
            })
        })
        .await
        .unwrap();

    // `prim ctl chat send` reaches the other member and this machine's own window.
    let (ok, total, failed) = send_chat_once(&a.socket, "from a script", Duration::from_secs(5)).await.unwrap();
    assert_eq!((ok, total, failed.len()), (1, 1, 0));
    b.win.wait_until(5, "script line at bob", || lines(&b.win).contains(&("alice".into(), "from a script".into()))).await.unwrap();
    a.win.wait_until(5, "script line at own window", || lines(&a.win).contains(&("alice".into(), "from a script".into()))).await.unwrap();

    // A late joiner sees nothing from before it joined.
    let c = f.join("carol", &a).await;
    c.win.wait_until(15, "carol sees roster", || online_names(&c.win).len() == 3).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        lines(&c.win).iter().all(|(_, t)| t != "test" && t != "from a script"),
        "late joiner got history: {:?}",
        lines(&c.win)
    );
    c.win.chat("hi from carol");
    for h in [&a.win, &b.win] {
        h.wait_until(5, "carol's line", || lines(h).contains(&("carol".into(), "hi from carol".into()))).await.unwrap();
    }

    // A stranger with a room of their own cannot post into ours.
    let d = f.found("mallory").await;
    d.win.wait_for(5, "mallory in own room", |m| m.in_room()).await.unwrap();
    d.win.chat("let me in");
    tokio::time::sleep(Duration::from_millis(800)).await;
    for h in [&a.win, &b.win, &c.win] {
        assert!(lines(h).iter().all(|(_, t)| t != "let me in"), "non-member chat delivered");
    }

    // A removed member's chat is not delivered either.
    let c_key = c.win.lock().me().unwrap().to_string();
    let mut ctl = Client::connect(&a.socket).await.unwrap();
    match ctl.call(|req| Request::Revoke { req, subject: c_key.clone() }).await.unwrap() {
        ReplyJson::Ok => {}
        other => panic!("revoke: {other:?}"),
    }
    a.win
        .wait_until(10, "carol gone from alice's roster", || !online_names(&a.win).contains(&"carol".to_string()))
        .await
        .unwrap();
    c.win.chat("still here?");
    tokio::time::sleep(Duration::from_millis(800)).await;
    for h in [&a.win, &b.win] {
        assert!(lines(h).iter().all(|(_, t)| t != "still here?"), "removed member's chat delivered");
    }

    // Voice: launching sets the member voice indicator, muting and stopping change/clear it.
    let mut voice = Voice::new(&a.socket);
    voice.start(false);
    let voice_of = |h: &Handle, name: &str| h.lock().members().into_iter().find(|r| r.name == name).and_then(|r| r.voice);
    b.win.wait_until(10, "alice listening, mic muted", || voice_of(&b.win, "alice") == Some((true, false))).await.unwrap();
    a.win.wait_until(5, "lease visible in status", || a.win.lock().voice_lease_held()).await.unwrap();
    voice.restart_with(true, false);
    b.win.wait_until(10, "alice mic live", || voice_of(&b.win, "alice") == Some((true, true))).await.unwrap();
    // Deafen is carried by the window's own `clients` key.
    a.win.send(Cmd::SetClients(Clients { deaf: true, ..Default::default() }));
    b.win.wait_for(5, "alice deafened", |m| m.members().iter().any(|r| r.name == "alice" && r.clients.deaf)).await.unwrap();
    // Muting again replaces the client; the indicator must follow (live -> muted).
    voice.restart_with(false, false);
    b.win.wait_until(10, "alice muted again", || voice_of(&b.win, "alice") == Some((true, false))).await.unwrap();
    voice.stop();
    b.win
        .wait_until(10, "alice voice cleared", || voice_of(&b.win, "alice").is_none_or(|(l, m)| !l && !m))
        .await
        .unwrap();
    a.win.wait_until(5, "lease released in status", || !a.win.lock().voice_lease_held()).await.unwrap();

    // The lease expires when the client is frozen; the client reconnects itself.
    let mut voice = Voice::new(&a.socket);
    voice.start(false);
    b.win.wait_until(10, "voice back", || voice_of(&b.win, "alice") == Some((true, false))).await.unwrap();
    let pids = supervise::find_procs("voice", "rt-socket", &a.socket);
    assert_eq!(pids.len(), 1, "one voice client: {pids:?}");
    // SAFETY: signalling our own child.
    unsafe { libc::kill(pids[0], libc::SIGSTOP) };
    // The lease must really expire while the process is frozen (TTL 2 s): watch the
    // daemon's holder go away, so a pass cannot mean "the freeze was too short to matter".
    let mut expired = false;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        expired |= !a.win.lock().voice_lease_held();
    }
    assert!(expired, "the frozen client's lease never expired");
    unsafe { libc::kill(pids[0], libc::SIGCONT) };
    // Round VC: a lost lease no longer ends the client. It reconnects inside the same
    // process (same pid, same incarnation), so nothing is relaunched.
    for _ in 0..30 {
        voice.tick();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let now = supervise::find_procs("voice", "rt-socket", &a.socket);
    assert!(
        matches!(voice.sup.state, ProcState::Running) && now == pids,
        "voice should have reconnected in place after lease expiry: {:?} {now:?} vs {pids:?}",
        voice.sup.state
    );
    // ... and the same process took the lease again.
    a.win.wait_until(5, "lease held again", || a.win.lock().voice_lease_held()).await.unwrap();
    b.win.wait_until(10, "voice listening again", || voice_of(&b.win, "alice") == Some((true, false))).await.unwrap();
    voice.stop();

    for n in [a, b, c, d] {
        n.daemon.shutdown().await;
    }
}
