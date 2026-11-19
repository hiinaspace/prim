//! Round GW oracle (ported to `prim-client` in round A1): the client layer every front
//! end uses (the Godot client, the dev window, `prim ctl`), against real daemons over
//! loopback iroh. No window is opened; `prim_client::room::Handle` is the same code Godot
//! reads every frame, and voice runs under a real `prim session` process. Voice uses the private test sinks (a tone "microphone", a null
//! output): never real speakers or a real microphone.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use prim_room::daemon::{Daemon, DaemonOpts, Start};
use prim_room::local::{Client, ReplyJson, Request};
use prim_room::transport::iroh::{IrohConfig, IrohTransport, PeerAddrs, RelaySetting};
use prim_client::room::{send_chat_once, Cmd, Delivery, Handle, Model, Presence, RoomOpts};
use prim_client::session::{SessionClient, SessionRequest};
use prim_client::transport::UnixTransport;
use room_core::api::Timing;

/// The blocking waits of `Handle`, usable from async tests without stalling the runtime.
trait WaitAsync {
    async fn wait_for_a(&self, secs: u64, what: &str, f: impl FnMut(&Model) -> bool) -> Result<(), String>;
    async fn wait_until_a(&self, secs: u64, what: &str, f: impl FnMut() -> bool) -> Result<(), String>;
}
impl WaitAsync for Handle {
    async fn wait_for_a(&self, secs: u64, what: &str, f: impl FnMut(&Model) -> bool) -> Result<(), String> {
        tokio::task::block_in_place(|| self.wait_for(secs, what, f))
    }
    async fn wait_until_a(&self, secs: u64, what: &str, f: impl FnMut() -> bool) -> Result<(), String> {
        tokio::task::block_in_place(|| self.wait_until(secs, what, f))
    }
}

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
        // The session spawns the real `prim` binary for voice.
        std::env::set_var("PRIM_BIN", env!("CARGO_BIN_EXE_prim"));
        std::env::set_var("PRIM_SESSION_VOICE_TEST", "1");
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_STATE_HOME", dir.path().join("xdg-state"));
        Fleet {
            dir,
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
        let win = Handle::attach(
            UnixTransport::new(&socket),
            RoomOpts {
                chat: true,
                default_name: Some(name.to_string()),
                ..Default::default()
            },
        );
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
            .wait_for_a(5, "invite", |m| m.invite.as_ref().is_some_and(|i| i.0 > before))
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
    a.win.wait_for_a(5, "alice alone", |m| m.in_room()).await.unwrap();
    a.win
        .wait_for_a(5, "alice named", |m| m.members().iter().any(|r| r.is_me && r.name == "alice"))
        .await
        .unwrap();
    assert_eq!(online_names(&a.win), ["alice"]);

    // Create + invite + join: both rosters converge, with names and a route.
    let b = f.join("bob", &a).await;
    for (who, h) in [("alice", &a.win), ("bob", &b.win)] {
        h.wait_until_a(15, &format!("{who} sees both"), || online_names(h) == ["alice", "bob"])
            .await
            .unwrap();
    }
    a.win
        .wait_for_a(10, "route to bob", |m| {
            m.members().iter().any(|r| r.name == "bob" && r.route.as_ref().is_some_and(|rt| rt.path != ""))
        })
        .await
        .unwrap();
    assert!(a.win.lock().members().iter().any(|r| r.is_me && r.keyholder), "opener is a keyholder");
    assert!(b.win.lock().members().iter().all(|r| !r.keyholder || r.name == "alice"));

    // Chat both ways, with delivery results.
    a.win.chat("test");
    b.win.wait_until_a(5, "bob gets test", || lines(&b.win).contains(&("alice".into(), "test".into()))).await.unwrap();
    b.win.chat("something's not working");
    a.win
        .wait_until_a(5, "alice gets reply", || lines(&a.win).contains(&("bob".into(), "something's not working".into())))
        .await
        .unwrap();
    a.win
        .wait_for_a(5, "delivery receipt", |m| {
            m.chat.iter().any(|l| {
                l.text == "test"
                    && matches!(&l.delivery, Some(Delivery::Done { ok: 1, total: 1, failed }) if failed.is_empty())
            })
        })
        .await
        .unwrap();

    // `prim ctl chat send` reaches the other member and this machine's own window.
    let (ok, total, failed) = tokio::task::block_in_place(|| send_chat_once(UnixTransport::new(&a.socket), "from a script", Duration::from_secs(5))).unwrap();
    assert_eq!((ok, total, failed.len()), (1, 1, 0));
    b.win.wait_until_a(5, "script line at bob", || lines(&b.win).contains(&("alice".into(), "from a script".into()))).await.unwrap();
    a.win.wait_until_a(5, "script line at own window", || lines(&a.win).contains(&("alice".into(), "from a script".into()))).await.unwrap();

    // A late joiner sees nothing from before it joined.
    let c = f.join("carol", &a).await;
    c.win.wait_until_a(15, "carol sees roster", || online_names(&c.win).len() == 3).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        lines(&c.win).iter().all(|(_, t)| t != "test" && t != "from a script"),
        "late joiner got history: {:?}",
        lines(&c.win)
    );
    c.win.chat("hi from carol");
    for h in [&a.win, &b.win] {
        h.wait_until_a(5, "carol's line", || lines(h).contains(&("carol".into(), "hi from carol".into()))).await.unwrap();
    }

    // A stranger with a room of their own cannot post into ours.
    let d = f.found("mallory").await;
    d.win.wait_for_a(5, "mallory in own room", |m| m.in_room()).await.unwrap();
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
        .wait_until_a(10, "carol gone from alice's roster", || !online_names(&a.win).contains(&"carol".to_string()))
        .await
        .unwrap();
    c.win.chat("still here?");
    tokio::time::sleep(Duration::from_millis(800)).await;
    for h in [&a.win, &b.win] {
        assert!(lines(h).iter().all(|(_, t)| t != "still here?"), "removed member's chat delivered");
    }

    // Voice: a real `prim session` adopts alice's daemon and runs voice for it. Starting
    // voice sets the member indicator; mic and stop change/clear it.
    let mut session = SessionGuard(std::process::Command::new(env!("CARGO_BIN_EXE_prim"))
        .args(["session", "--socket"])
        .arg(&a.socket)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap(), a.socket.clone());
    let sc = SessionClient::new(&a.socket);
    let mut tries = 0;
    let st = loop {
        match tokio::task::block_in_place(|| sc.status()) {
            Ok(st) => break st,
            Err(e) if tries > 100 => panic!("session did not come up: {e}"),
            Err(_) => {
                tries += 1;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    };
    assert_eq!(st.room, "up");
    assert_eq!(st.kind, "attached", "the session adopts a daemon that is already running");
    let call = |r: SessionRequest| tokio::task::block_in_place(|| sc.call(&r)).unwrap();
    let voice_of = |h: &Handle, name: &str| h.lock().members().into_iter().find(|r| r.name == name).and_then(|r| r.voice);
    call(SessionRequest::Voice { on: true });
    b.win.wait_until_a(10, "alice listening, mic muted", || voice_of(&b.win, "alice") == Some((true, false))).await.unwrap();
    a.win.wait_until_a(5, "lease visible in status", || a.win.lock().voice_lease_held()).await.unwrap();
    let first = call(SessionRequest::Status);
    let holder0 = a.win.lock().voice_holder().expect("voice lease holder");
    let (vpid, vstarts) = (first.voice.pid.expect("voice pid"), first.voice.starts);
    // Round VC: mic and deafen are requests to the running client. Every toggle below must
    // keep the same process and the same start count, and the peer's indicator must follow.
    let same = |what: &str, st: &prim_client::session::SessionStatus| {
        assert!(
            st.voice.running && st.voice.pid == Some(vpid) && st.voice.starts == vstarts,
            "{what}: voice was restarted: {:?}",
            st.voice
        );
    };
    let st = call(SessionRequest::SetMic { on: true });
    assert!(st.voice.mic && !st.voice.deaf);
    same("mic on", &st);
    b.win.wait_until_a(10, "alice mic live", || voice_of(&b.win, "alice") == Some((true, true))).await.unwrap();
    let st = call(SessionRequest::SetDeafen { on: true });
    assert!(st.voice.deaf && st.voice.mic, "{:?}", st.voice);
    same("deafen on", &st);
    b.win.wait_until_a(10, "alice deafened (not listening, mic live)", || voice_of(&b.win, "alice") == Some((false, true))).await.unwrap();
    let st = call(SessionRequest::SetDeafen { on: false });
    assert!(!st.voice.deaf && st.voice.mic, "{:?}", st.voice);
    same("deafen off", &st);
    b.win.wait_until_a(10, "alice hearing again", || voice_of(&b.win, "alice") == Some((true, true))).await.unwrap();
    let st = call(SessionRequest::SetMic { on: false });
    assert!(!st.voice.mic);
    same("mic off", &st);
    b.win.wait_until_a(10, "alice muted again", || voice_of(&b.win, "alice") == Some((true, false))).await.unwrap();
    for on in [true, false] {
        let st = call(SessionRequest::SetMic { on });
        same("mic toggle", &st);
        b.win.wait_until_a(10, "alice indicator follows", || voice_of(&b.win, "alice") == Some((true, on))).await.unwrap();
    }
    // The room key must show one incarnation throughout (peers' receivers never reset).
    tokio::time::sleep(Duration::from_millis(1300)).await; // status is cached up to 1 s
    let now = call(SessionRequest::Status);
    same("after all toggles", &now);
    assert_eq!(a.win.lock().voice_holder(), Some(holder0), "voice session/incarnation changed across toggles");
    call(SessionRequest::Voice { on: false });
    b.win
        .wait_until_a(10, "alice voice cleared", || voice_of(&b.win, "alice").is_none_or(|(l, m)| !l && !m))
        .await
        .unwrap();
    a.win.wait_until_a(5, "lease released in status", || !a.win.lock().voice_lease_held()).await.unwrap();

    // The lease expires when the client is frozen; the client reconnects itself.
    call(SessionRequest::Voice { on: true });
    // Unmute then mute straight after turning voice on, while the new client may not have a
    // lease yet (each call waits up to 3 s for voice's acknowledgement). It must end muted.
    // The ordering guarantees themselves are unit-tested in prim-session's voice.rs.
    call(SessionRequest::SetMic { on: true });
    call(SessionRequest::SetMic { on: false });
    b.win.wait_until_a(10, "voice back", || voice_of(&b.win, "alice") == Some((true, false))).await.unwrap();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let early = call(SessionRequest::Status);
    assert!(!early.voice.mic, "mute during startup was lost: {:?}", early.voice);
    assert_eq!(voice_of(&b.win, "alice"), Some((true, false)), "peer sees a live mic after a startup mute");
    let st2 = call(SessionRequest::Status);
    let pid = st2.voice.pid.expect("voice pid") as i32;
    let starts = st2.voice.starts;
    // SAFETY: signalling the voice client this test's session started.
    unsafe { libc::kill(pid, libc::SIGSTOP) };
    // The lease must really expire while the process is frozen (TTL 2 s): watch the
    // daemon's holder go away, so a pass cannot mean "the freeze was too short to matter".
    let mut expired = false;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        expired |= !a.win.lock().voice_lease_held();
    }
    assert!(expired, "the frozen client's lease never expired");
    unsafe { libc::kill(pid, libc::SIGCONT) };
    // Round VC: a lost lease no longer ends the client. It reconnects inside the same
    // process (same pid), so the session must not relaunch it.
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let now = call(SessionRequest::Status);
    assert!(
        now.voice.running && now.voice.state == "running" && now.voice.pid == Some(pid as u32) && now.voice.starts == starts,
        "voice should have reconnected in place after lease expiry: {:?}",
        now.voice
    );
    // ... and the same process took the lease again.
    a.win.wait_until_a(5, "lease held again", || a.win.lock().voice_lease_held()).await.unwrap();
    b.win.wait_until_a(10, "voice listening again", || voice_of(&b.win, "alice") == Some((true, false))).await.unwrap();
    // A voice client that dies with no front end attached comes back muted, never live.
    call(SessionRequest::SetMic { on: true });
    b.win.wait_until_a(10, "alice mic live before the crash", || voice_of(&b.win, "alice") == Some((true, true))).await.unwrap();
    let dying = call(SessionRequest::Status);
    // SAFETY: killing the voice client this test's session started.
    unsafe { libc::kill(dying.voice.pid.expect("voice pid") as i32, libc::SIGKILL) };
    let mut relaunched = None;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let st = call(SessionRequest::Status);
        if st.voice.running && st.voice.pid != dying.voice.pid {
            relaunched = Some(st);
            break;
        }
    }
    let relaunched = relaunched.expect("the session did not relaunch voice after kill -9");
    assert!(!relaunched.voice.mic, "relaunched voice is not muted: {:?}", relaunched.voice);
    b.win.wait_until_a(10, "relaunch is muted for peers", || voice_of(&b.win, "alice") == Some((true, false))).await.unwrap();
    call(SessionRequest::Quit);
    let _ = session.0.wait();

    for n in [a, b, c, d] {
        n.daemon.shutdown().await;
    }
}

/// Ends the `prim session` this test started and its voice client, even when an assertion
/// fails: a clean Quit first; if the session is stuck, kill it and then the voice pid it
/// last reported, so a red run leaves no orphan voice process behind.
struct SessionGuard(std::process::Child, PathBuf);

impl Drop for SessionGuard {
    fn drop(&mut self) {
        let client = SessionClient::new(&self.1);
        let voice_pid = client.status().ok().and_then(|st| st.voice.pid);
        let _ = client.call(&SessionRequest::Quit);
        std::thread::sleep(Duration::from_millis(500));
        let _ = self.0.kill();
        let _ = self.0.wait();
        if let Some(pid) = voice_pid {
            // SAFETY: SIGTERM to the voice client of the session this test started.
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        }
    }
}
