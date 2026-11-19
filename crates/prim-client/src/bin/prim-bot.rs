//! Bot peer for the cowatch loop: attaches to its own room daemon, waits until it is in
//! the room, then writes a moving `member/<id>/pose` live record (an orbit) at `--hz`.
//!
//! prim-bot --socket PATH [--events FILE] [--hz 20] [--seconds N] [--stop-after S] [--watch FILE] [--name bot]
//!
//! Pose payload: the 64-byte binary record of `prim_client::pose` (C2). The pose is a known
//! function of time `t = seq / hz`, so an oracle can recompute it independently:
//!   a = 0.6 t;  head = (0.9 sin a, 1.65, -1.4 + 0.2 cos a), yaw = pi + 0.5 sin a (about +Y)
//!   in the head's yaw frame (person's right = +x, forward = -z):
//!   LEFT hand raised: (-0.30, +0.25 + 0.05 sin 3a, -0.30); RIGHT hand low: (+0.30, -0.55, -0.15)
//! Extra flags: --stop-after S (stop publishing after S seconds but stay attached: the
//! record must then expire at viewers), --watch FILE (append every pose another member
//! publishes, decoded, to FILE as JSON lines: how a test reads what the Godot client published).

use std::time::{Duration, Instant};

use prim_client::pose::{self, Core, Pose};
use prim_client::wire::{unb64, Event, ReplyJson, UpdateJson};
use prim_client::transport::UnixTransport;
use prim_client::{Client, ClientEvent};
use prim_events::{ev, Sink};
use serde_json::json;

fn main() {
    let mut socket = None;
    let mut events = None;
    let mut hz = 20.0f64;
    let mut seconds = 0.0f64;
    let mut name = "bot".to_string();
    let mut stop_after = 0.0f64;
    let mut watch: Option<String> = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut v = || it.next().unwrap_or_else(|| usage());
        match a.as_str() {
            "--socket" => socket = Some(v()),
            "--events" => events = Some(v()),
            "--hz" => hz = v().parse().unwrap_or_else(|_| usage()),
            "--seconds" => seconds = v().parse().unwrap_or_else(|_| usage()),
            "--name" => name = v(),
            "--stop-after" => stop_after = v().parse().unwrap_or_else(|_| usage()),
            "--watch" => watch = Some(v()),
            _ => usage(),
        }
    }
    let socket = socket.unwrap_or_else(|| usage());
    let sink = match events {
        Some(p) => Sink::open("bot", std::path::Path::new(&p)).unwrap_or_else(|e| {
            eprintln!("prim-bot: events file: {e}");
            std::process::exit(2)
        }),
        None => Sink::from_env("bot"),
    };
    sink.emit(ev::SPAWN, json!({"what": "bot"}));
    let client = Client::start(UnixTransport::new(&socket));

    // Wait for the daemon, then for membership + a coordinator (state writes need a room).
    let deadline = Instant::now() + Duration::from_secs(60);
    let me = loop {
        if Instant::now() > deadline {
            eprintln!("prim-bot: never joined");
            std::process::exit(1);
        }
        if let Ok(ReplyJson::Status { status }) = client.call(
            |req| prim_client::wire::Request::Status { req },
            Duration::from_secs(2),
        ) {
            let open = status["lifecycle"] == "open";
            let members = status["members"].as_array().map(|m| m.len()).unwrap_or(0);
            if open && members >= 2 && status["coordinator"].is_object() {
                let me = status["me"].as_str().unwrap_or("").to_string();
                sink.emit(ev::JOINED, json!({"member": me, "members": members}));
                break me;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let _ = client.write(
        "member",
        "profile",
        false,
        false,
        json!({"name": name}).to_string().as_bytes(),
    );

    let watch_sub = watch.as_ref().and_then(|_| client.subscribe("member/*/pose").ok());
    let period = Duration::from_secs_f64(1.0 / hz);
    let start = Instant::now();
    let mut n = 0u64;
    let mut next = Instant::now();
    loop {
        let t = start.elapsed().as_secs_f64();
        if seconds > 0.0 && t > seconds {
            break;
        }
        let publishing = stop_after <= 0.0 || t <= stop_after;
        if publishing {
            let core = bot_pose(n as f64 / hz, (n & 0xffff) as u16);
            // Errors (daemon down) are expected during fault runs; keep trying.
            let _ = client.write("member", "pose", true, false, &pose::encode(&core));
        }
        n += 1;
        while let Some(e) = client.try_recv() {
            match e {
                ClientEvent::Event(Event::Error { message }) => eprintln!("prim-bot: daemon error: {message}"),
                ClientEvent::Event(Event::Updates { sub, updates }) if Some(sub) == watch_sub => {
                    for u in updates {
                        if let UpdateJson::Value { path, writer, value, .. } = u {
                            if writer == me || !path.ends_with("/pose") {
                                continue;
                            }
                            if let (Some(f), Ok(b)) = (&watch, unb64(&value)) {
                                if let Ok(c) = pose::decode(&b) {
                                    write_watch(f, &writer, &c);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        next += period;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
}

fn usage() -> ! {
    eprintln!("usage: prim-bot --socket PATH [--events FILE] [--hz 20] [--seconds N] [--stop-after S] [--watch FILE] [--name bot]");
    std::process::exit(2)
}

fn yaw_q(a: f64) -> [f32; 4] {
    [0.0, (a / 2.0).sin() as f32, 0.0, (a / 2.0).cos() as f32]
}

/// Rotate `v` about +Y by `yaw` (Godot: positive yaw turns -Z toward -X).
fn rot_y(yaw: f64, v: [f64; 3]) -> [f64; 3] {
    let (s, c) = yaw.sin_cos();
    [c * v[0] + s * v[2], v[1], -s * v[0] + c * v[2]]
}

/// The bot's pose at time `t` seconds (see the module doc; oracles recompute it).
pub fn bot_pose(t: f64, seq: u16) -> Core {
    let a = 0.6 * t;
    let head = [0.9 * a.sin(), 1.65, -1.4 + 0.2 * a.cos()];
    let yaw = std::f64::consts::PI + 0.5 * a.sin();
    let place = |local: [f64; 3]| {
        let r = rot_y(yaw, local);
        Pose { pos: [(head[0] + r[0]) as f32, (head[1] + r[1]) as f32, (head[2] + r[2]) as f32], rot: yaw_q(yaw) }
    };
    Core {
        flags: pose::FLAG_LEFT | pose::FLAG_RIGHT | pose::FLAG_VR,
        seq,
        head: Pose { pos: [head[0] as f32, head[1] as f32, head[2] as f32], rot: yaw_q(yaw) },
        left: place([-0.30, 0.25 + 0.05 * (3.0 * a).sin(), -0.30]),
        right: place([0.30, -0.55, -0.15]),
    }
}

fn write_watch(path: &str, writer: &str, c: &Core) {
    use std::io::Write;
    let p = |p: &Pose| json!({"pos": p.pos, "rot": p.rot});
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let body = json!({"t_unix_ms": ms, "writer": writer, "seq": c.seq, "flags": c.flags,
        "head": p(&c.head), "left": p(&c.left), "right": p(&c.right)});
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{body}");
    }
}
