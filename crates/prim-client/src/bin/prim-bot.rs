//! Bot peer for the cowatch loop: attaches to its own room daemon, waits until it is in
//! the room, then writes a moving `member/<id>/pose` live record (an orbit) at `--hz`.
//!
//! prim-bot --socket PATH [--events FILE] [--hz 20] [--seconds N] [--name bot]
//!
//! Pose payload (JSON, < 200 B live cap): {"v":1,"n":seq,"pos":[x,y,z],"yaw":rad}.
//! The Godot room reads the same shape; C2 replaces it with the 116 B core pose.

use std::time::{Duration, Instant};

use prim_client::wire::{Event, ReplyJson};
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
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut v = || it.next().unwrap_or_else(|| usage());
        match a.as_str() {
            "--socket" => socket = Some(v()),
            "--events" => events = Some(v()),
            "--hz" => hz = v().parse().unwrap_or_else(|_| usage()),
            "--seconds" => seconds = v().parse().unwrap_or_else(|_| usage()),
            "--name" => name = v(),
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
    let _ = &me;
    let _ = client.write(
        "member",
        "profile",
        false,
        false,
        json!({"name": name}).to_string().as_bytes(),
    );

    let period = Duration::from_secs_f64(1.0 / hz);
    let start = Instant::now();
    let mut n = 0u64;
    let mut next = Instant::now();
    loop {
        let t = start.elapsed().as_secs_f64();
        if seconds > 0.0 && t > seconds {
            break;
        }
        // Orbit: radius 1 m, 0.6 rad/s (0.6 m/s), centred 0.6 m in front of the origin.
        let a = t * 0.6;
        let pos = [0.9 * a.sin(), 0.0, -1.4 + 0.2 * a.cos()];
        // Facing along the direction of travel (marker forward is -z rotated by yaw).
        let yaw = if a.cos() >= 0.0 { -std::f64::consts::FRAC_PI_2 } else { std::f64::consts::FRAC_PI_2 };
        let body = json!({"v": 1, "n": n, "pos": pos, "yaw": yaw}).to_string();
        // Errors (daemon down) are expected during fault runs; keep trying.
        let _ = client.write("member", "pose", true, false, body.as_bytes());
        n += 1;
        while let Some(e) = client.try_recv() {
            if let ClientEvent::Event(Event::Error { message }) = e {
                eprintln!("prim-bot: daemon error: {message}");
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
    eprintln!("usage: prim-bot --socket PATH [--events FILE] [--hz 20] [--seconds N] [--name bot]");
    std::process::exit(2)
}
