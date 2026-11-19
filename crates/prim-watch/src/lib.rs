//! `prim watch`: a standalone mpv that cowatches through the room daemon.
//!
//! The pieces: [`mpv`] (child process and JSON IPC), [`player`] (mpv as a cowatch
//! player), and this host loop, which carries `prim-client` events into
//! `prim_cowatch::Cowatch` and its actions back out. Single thread; 10 ms granularity,
//! a report and tick every 100 ms.

pub mod mpv;
pub mod player;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use data_encoding::{BASE64, HEXLOWER};
use prim_client::transport::UnixTransport;
use prim_client::wire::{Event as W, ReplyJson, Request, UpdateJson};
use prim_client::{Client, ClientEvent};
use prim_cowatch::wire::{Key, CH_CONTROL, CH_PROBE};
use prim_cowatch::{
    Action, Config, CoordState, CoordView, Cowatch, Dest, Event, MemberState, Notice, SendOutcome,
    UserCommand, WriteScope,
};

pub const VALUED_FLAGS: &[&str] = &["socket", "mpv", "mpv-arg", "ipc", "tick-ms"];

const HELP: &str = "\
prim watch [options] [URL-or-file]

Plays the same video as everyone else in the room, in mpv. With a URL or file, loads
it for the room; without, joins whatever the room is watching. Pause, seek and the
mpv window's own keys change it for everyone. Close mpv or press ctrl-C to leave.

options:
  --socket PATH    room daemon socket (default $PRIM_SOCKET or $XDG_RUNTIME_DIR/prim/room.sock)
  --mpv PATH       mpv program (default $PRIM_MPV or mpv from PATH)
  --mpv-arg ARG    extra mpv argument (repeatable; also $PRIM_WATCH_MPV_ARGS, space separated)
  --ipc PATH       where mpv's JSON IPC socket goes (default beside the room socket)
";

pub trait Flags {
    fn positional(&self) -> &[String];
    fn has(&self, name: &str) -> bool;
    fn value(&self, name: &str) -> Option<&str>;
    fn values(&self, name: &str) -> Vec<&str>;
}

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_term(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

fn hex_key(s: &str) -> Option<Key> {
    HEXLOWER.decode(s.as_bytes()).ok()?.try_into().ok()
}

fn hex(k: &Key) -> String {
    HEXLOWER.encode(k)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Clone, Copy)]
enum Pending {
    Write(u64),
    Send(u64),
    Status,
}

struct Host {
    client: Client,
    cw: Cowatch,
    start: Instant,
    pending: HashMap<u64, Pending>,
    msg_tokens: HashMap<u64, u64>,
    subs: HashMap<u32, &'static str>,
    /// Subscriptions whose first snapshot has arrived.
    snaps: std::collections::HashSet<&'static str>,
    synced: bool,
    player: player::Player,
    verbose: bool,
}

fn status_coord(status: &serde_json::Value) -> Option<CoordView> {
    let c = status.get("coordinator")?;
    Some(CoordView {
        generation: c.get("generation")?.as_u64()? as u32,
        term: c.get("term")?.as_u64()? as u32,
        holder: hex_key(c.get("holder")?.as_str()?)?,
        state: CoordState::Live,
    })
}

fn outcome(s: &str) -> SendOutcome {
    match s {
        "delivered" => SendOutcome::Delivered,
        "no_channel" => SendOutcome::NoChannel,
        "not_member" => SendOutcome::NotMember,
        "too_large" => SendOutcome::TooLarge,
        "lost" => SendOutcome::Lost,
        "offline" => SendOutcome::Offline,
        _ => SendOutcome::Other,
    }
}

fn mono_us() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: plain syscall writing to a valid timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000 + ts.tv_nsec as u64 / 1000
}

impl Host {
    fn now_us(&self) -> u64 {
        // Host-wide monotonic clock, not time since this process started: a watcher restarted
        // on the same daemon keeps the same clock identity in its peers' estimators, so its
        // timestamps must stay in the same timebase.
        mono_us()
    }

    /// Feed an event, carry out the actions, and feed what they cause, until quiet.
    fn feed(&mut self, ev: Event) {
        if self.verbose {
            if let Event::SendResult { token, outcome } = &ev {
                eprintln!("prim watch: t={}ms send {token} -> {outcome:?}", self.now_us() / 1000);
            }
            if let Event::Member { key, state } = &ev {
                eprintln!("prim watch: member {:02x}{:02x} {state:?}", key[0], key[1]);
            }
            if let Event::Coordinator(c) = &ev {
                eprintln!("prim watch: coordinator {c:?}");
            }
        }
        let mut q = vec![ev];
        while let Some(ev) = q.pop() {
            let acts = self.cw.handle(self.now_us(), ev);
            self.exec(acts, &mut q);
        }
    }

    fn exec(&mut self, acts: Vec<Action>, q: &mut Vec<Event>) {
        for a in acts {
            match a {
                Action::Write { token, scope, name, value } => {
                    let scope = match scope {
                        WriteScope::Room => "room",
                        WriteScope::Member => "member",
                    };
                    match self.client.write(scope, name, false, false, &value) {
                        Ok(req) => {
                            self.pending.insert(req, Pending::Write(token));
                        }
                        Err(_) => q.push(Event::WriteResult { token, ok: false }),
                    }
                }
                Action::Send { token, to, payload } => {
                    let to = match to {
                        Dest::Coordinator => "coordinator".to_string(),
                        Dest::Member(k) => hex(&k),
                    };
                    match self.client.send(&to, CH_CONTROL, true, &payload) {
                        Ok(req) => {
                            self.pending.insert(req, Pending::Send(token));
                        }
                        Err(_) => q.push(Event::SendResult { token, outcome: SendOutcome::Offline }),
                    }
                }
                Action::Datagram { to, payload } => {
                    let _ = self.client.send_datagram(&hex(&to), CH_PROBE, &payload, 500);
                }
                Action::Player(c) => {
                    if self.verbose {
                        eprintln!("prim watch: player {c:?}");
                    }
                    self.player.apply(&c);
                }
                Action::Notice(n) => self.notice(n),
            }
        }
    }

    fn notice(&mut self, n: Notice) {
        eprintln!("prim watch: notice {n:?}");
        let text = match n {
            Notice::Finished => return,
            Notice::Reloading { attempt, of, .. } => format!("Connection dropped; reloading ({attempt}/{of})"),
            Notice::Failed => "Could not keep the video going".to_string(),
            Notice::ControlUnavailable => "Nobody in the room can take control right now".to_string(),
            Notice::NoAuthority | Notice::AuthorityFound => return,
        };
        self.player.mpv.show_text(&text, 3000);
    }

    fn refresh_status(&mut self) {
        if let Ok(req) = self.client.status() {
            self.pending.insert(req, Pending::Status);
        }
    }

    fn client_event(&mut self, e: ClientEvent) -> Result<()> {
        match e {
            ClientEvent::Connected => self.refresh_status(),
            ClientEvent::Disconnected | ClientEvent::Unknown(_) => {}
            ClientEvent::Event(ev) => self.daemon_event(ev)?,
        }
        Ok(())
    }

    fn daemon_event(&mut self, ev: W) -> Result<()> {
        match ev {
            W::Reply { req, reply } => match (self.pending.remove(&req), reply) {
                (Some(Pending::Write(token)), r) => self.feed(Event::WriteResult { token, ok: r == ReplyJson::Ok }),
                (Some(Pending::Send(token)), ReplyJson::Sending { msg_id, targets }) => {
                    if targets.is_empty() {
                        self.feed(Event::SendResult { token, outcome: SendOutcome::NoCoordinator });
                    } else {
                        self.msg_tokens.insert(msg_id, token);
                    }
                }
                (Some(Pending::Send(token)), ReplyJson::Error { code }) => {
                    let o = match code.as_str() {
                        "no_coordinator" => SendOutcome::NoCoordinator,
                        "offline" => SendOutcome::Offline,
                        _ => SendOutcome::Other,
                    };
                    self.feed(Event::SendResult { token, outcome: o });
                }
                (Some(Pending::Send(token)), _) => self.feed(Event::SendResult { token, outcome: SendOutcome::Other }),
                (Some(Pending::Status), ReplyJson::Status { status }) => {
                    self.feed(Event::Coordinator(status_coord(&status)));
                }
                _ => {}
            },
            W::MessageResult { msg_id, status, .. } => {
                if let Some(token) = self.msg_tokens.remove(&msg_id) {
                    self.feed(Event::SendResult { token, outcome: outcome(&status) });
                }
            }
            W::Message { from, channel, payload } if channel == CH_CONTROL => {
                if let (Some(from), Ok(payload)) = (hex_key(&from), BASE64.decode(payload.as_bytes())) {
                    self.feed(Event::Message { from, payload });
                }
            }
            W::Datagram { from, channel, payload, .. } if channel == CH_PROBE => {
                if let (Some(from), Ok(payload)) = (hex_key(&from), BASE64.decode(payload.as_bytes())) {
                    self.feed(Event::Datagram { from, payload });
                }
            }
            W::Member { key, what } => {
                if let Some(key) = hex_key(&key) {
                    let state = match what.as_str() {
                        "joined" | "online" => MemberState::Online,
                        "offline" => MemberState::Offline,
                        "unresponsive" => MemberState::Unresponsive,
                        _ => MemberState::Removed,
                    };
                    self.feed(Event::Member { key, state });
                }
            }
            W::Coordinator { change, generation, term, holder } => {
                if let Some(holder) = hex_key(&holder) {
                    let state = match change.as_str() {
                        "left" => CoordState::Left,
                        "unresponsive" => CoordState::Unresponsive,
                        _ => CoordState::Live,
                    };
                    self.feed(Event::Coordinator(Some(CoordView { generation, term, holder, state })));
                }
            }
            W::Updates { sub, updates } => {
                let Some(kind) = self.subs.get(&sub).copied() else { return Ok(()) };
                for u in updates {
                    self.update(kind, u);
                }
            }
            W::Removed => bail!("this computer was removed from the room"),
            _ => {}
        }
        Ok(())
    }

    fn update(&mut self, kind: &'static str, u: UpdateJson) {
        let room = kind == "room";
        match u {
            UpdateJson::Reset => self.feed(if kind == "cap" {
                Event::CapsReset
            } else {
                Event::PlaybackReset { room }
            }),
            UpdateJson::Value { writer, value, .. } => {
                let (Some(writer), Ok(value)) = (hex_key(&writer), BASE64.decode(value.as_bytes())) else {
                    return;
                };
                self.feed(match (kind, value.is_empty()) {
                    ("cap", true) => Event::CapRemoved { writer },
                    ("cap", false) => Event::Cap { writer, value },
                    (_, true) => Event::PlaybackRemoved { room, writer: Some(writer) },
                    (_, false) => Event::Playback { room, writer, value },
                });
            }
            UpdateJson::Removed { path } => {
                let w = path.split('/').nth(1).and_then(hex_key);
                self.feed(match (kind, w) {
                    ("cap", Some(writer)) => Event::CapRemoved { writer },
                    ("cap", None) => return,
                    _ => Event::PlaybackRemoved { room, writer: if room { None } else { w } },
                });
            }
            UpdateJson::SnapshotDone => {
                self.snaps.insert(kind);
                if !self.synced && self.snaps.len() == 3 {
                    self.synced = true;
                    self.feed(Event::Synced);
                }
            }
        }
    }
}

pub fn run(args: &dyn Flags) -> Result<ExitCode> {
    if args.has("help") {
        eprint!("{HELP}");
        return Ok(ExitCode::from(2));
    }
    let socket: PathBuf = args
        .value("socket")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("PRIM_SOCKET").filter(|v| !v.is_empty()).map(PathBuf::from))
        .unwrap_or_else(prim_room::local::default_socket_path);
    let mpv_exe = args
        .value("mpv")
        .map(str::to_string)
        .or_else(|| std::env::var("PRIM_MPV").ok().filter(|v| !v.is_empty()))
        .unwrap_or_else(|| "mpv".into());
    let mut mpv_args: Vec<String> = std::env::var("PRIM_WATCH_MPV_ARGS")
        .map(|s| s.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    mpv_args.extend(args.values("mpv-arg").into_iter().map(str::to_string));
    let ipc = args
        .value("ipc")
        .map(PathBuf::from)
        .unwrap_or_else(|| socket.with_file_name(format!("mpv-{}.sock", std::process::id())));
    let source = args
        .positional()
        .first()
        .cloned()
        .filter(|s| !s.trim().is_empty())
        .map(|s| player::normalize_source(&s));
    let verbose = std::env::var_os("PRIM_WATCH_DEBUG").is_some();

    // SAFETY: the handler only stores to an atomic.
    unsafe {
        libc::signal(libc::SIGTERM, on_term as *const () as usize);
        libc::signal(libc::SIGINT, on_term as *const () as usize);
    }

    let client = Client::start(UnixTransport::new(&socket));
    let t0 = Instant::now();
    loop {
        match client.recv_timeout(Duration::from_millis(100)) {
            Some(ClientEvent::Connected) => break,
            _ if t0.elapsed() > Duration::from_secs(10) => {
                bail!("no room daemon answering on {} (start `prim room` first)", socket.display())
            }
            _ => {}
        }
    }
    let status = match client.call(|req| Request::Status { req }, Duration::from_secs(5))? {
        ReplyJson::Status { status } => status,
        other => bail!("status: {other:?}"),
    };
    let me = status
        .get("me")
        .and_then(|v| v.as_str())
        .and_then(hex_key)
        .context("daemon status has no member key")?;

    let mpv = mpv::Mpv::spawn(&mpv_exe, &ipc, &mpv_args)?;
    let player = player::Player::new(mpv);

    for ch in [CH_CONTROL, CH_PROBE] {
        client.register_channel(ch)?;
    }
    let mut subs = HashMap::new();
    for (pat, kind) in [("room/playback", "room"), ("member/*/playback", "member"), ("member/*/cowatch", "cap")] {
        subs.insert(client.subscribe(pat)?, kind);
    }

    let start = Instant::now();
    let mut cfg = Config::new(me, now_ms());
    // Test hook: the correction loop off, to show the oracles can see an offset.
    if std::env::var("PRIM_WATCH_FAULT").as_deref() == Ok("no_correction") {
        cfg.faults.no_correction = true;
    }
    let (cw, boot) = Cowatch::new(cfg, mono_us());
    let mut host = Host {
        client,
        cw,
        start,
        pending: HashMap::new(),
        msg_tokens: HashMap::new(),
        subs,
        snaps: Default::default(),
        synced: false,
        player,
        verbose,
    };
    let mut q = vec![];
    host.exec(boot, &mut q);
    for ev in q {
        host.feed(ev);
    }
    host.feed(Event::Coordinator(status_coord(&status)));
    if let Some(src) = source {
        host.feed(Event::Command(UserCommand::Load {
            kind: player::kind_of(&src),
            source: src.into_bytes(),
        }));
    }
    eprintln!("prim watch: ready (mpv ipc {})", ipc.display());

    let tick = Duration::from_millis(
        args.value("tick-ms").and_then(|v| v.parse().ok()).unwrap_or(100),
    );
    let mut last_tick = Instant::now() - tick;
    let mut last_view = Instant::now();
    let result: Result<()> = 'main: loop {
        if STOP.load(Ordering::Relaxed) {
            break Ok(());
        }
        let mut idle = true;
        while let Some(e) = host.client.try_recv() {
            idle = false;
            if let Err(e) = host.client_event(e) {
                break 'main Err(e);
            }
        }
        let (cmds, gone) = host.player.poll();
        if gone {
            break Ok(());
        }
        for c in cmds {
            idle = false;
            if verbose {
                eprintln!("prim watch: person {c:?}");
            }
            host.feed(Event::Command(c));
        }
        if verbose && last_view.elapsed() >= Duration::from_secs(2) {
            last_view = Instant::now();
            eprintln!("prim watch: t={}ms state {}", host.now_us() / 1000, host.cw.debug_state());
        }
        if last_tick.elapsed() >= tick {
            last_tick = Instant::now();
            if host.player.has_src() {
                let rep = host.player.report();
                host.feed(Event::Player(rep));
            }
            host.feed(Event::Tick);
        } else if idle {
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    host.player.mpv.quit();
    result?;
    Ok(ExitCode::SUCCESS)
}
