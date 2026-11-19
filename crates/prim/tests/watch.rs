//! Round PM oracles: `prim watch` against real daemon processes (loopback, no relay) and
//! real mpv with `--vo=null --ao=null`: never a real window, speaker or the user's
//! room. Positions are read from each player's own mpv IPC socket, not from `prim watch`.
//!
//! Needs `mpv` (or $PRIM_MPV) and `ffmpeg` (to make the test clips). Skips with a
//! message when either is missing. Numbers are printed (run with `--nocapture`).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

fn prim() -> Command {
    Command::new(env!("CARGO_BIN_EXE_prim"))
}

fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
        || Command::new(tool)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
}

fn mpv_exe() -> String {
    std::env::var("PRIM_MPV").unwrap_or_else(|_| "mpv".into())
}

macro_rules! need_tools {
    () => {
        if !have(&mpv_exe()) || !have("ffmpeg") {
            eprintln!("SKIP: needs mpv and ffmpeg");
            return;
        }
    };
}

/// Slack for a loaded dev machine on top of the crate's claims.
/// A scratch directory; with PRIM_WATCH_KEEP set it is kept (and printed) for post-mortems.
fn testdir() -> Dir {
    let t = tempfile::tempdir().unwrap();
    if std::env::var_os("PRIM_WATCH_KEEP").is_some() {
        let p = t.keep();
        eprintln!("kept {}", p.display());
        return Dir(None, p);
    }
    let p = t.path().to_path_buf();
    Dir(Some(t), p)
}

struct Dir(Option<tempfile::TempDir>, PathBuf);

impl Dir {
    fn path(&self) -> &Path {
        &self.1
    }
}

fn load_factor() -> f64 {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse::<f64>().ok()))
        .unwrap_or(0.0)
}

fn wait_for(secs: f64, what: &str, mut f: impl FnMut() -> bool) -> f64 {
    let t = Instant::now();
    while t.elapsed().as_secs_f64() < secs {
        if f() {
            return t.elapsed().as_secs_f64();
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    panic!("timed out after {secs}s waiting for {what} (load {})", load_factor());
}

// ---- media ------------------------------------------------------------------------------

fn clip(dir: &Path, name: &str, secs: u32) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        let st = Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=320x240:rate=25"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440", "-t", &secs.to_string()])
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-g", "25", "-pix_fmt", "yuv420p"])
            .args(["-c:a", "aac", "-movflags", "+faststart"])
            .arg(&p)
            .status()
            .unwrap();
        assert!(st.success());
    }
    p
}

// ---- daemons ----------------------------------------------------------------------------

struct Daemon {
    child: Child,
    socket: PathBuf,
    key: String,
    addr: String,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_daemon(dir: &Path, name: &str, join: Option<(&str, &[Daemon])>) -> Daemon {
    let root = dir.join(name);
    std::fs::create_dir_all(&root).unwrap();
    let socket = root.join("room.sock");
    let out = root.join("out.txt");
    let mut c = prim();
    c.arg("room");
    match join {
        None => c.arg("found"),
        Some((tok, others)) => {
            c.args(["join", tok]);
            // no relay or discovery here: every earlier member's address by hand
            for o in others {
                c.arg(format!("--peer-addr={}={}", o.key, o.addr));
            }
            &mut c
        }
    };
    c.args(["--relay", "none", "--bind", "127.0.0.1:0"])
        .arg(format!("--socket={}", socket.display()))
        .arg(format!("--state-dir={}", root.join("state").display()))
        .stdout(std::fs::File::create(&out).unwrap())
        .stderr(Stdio::null());
    let child = c.spawn().unwrap();
    let mut d = Daemon { child, socket, key: String::new(), addr: String::new() };
    wait_for(20.0, "daemon ready", || {
        let t = std::fs::read_to_string(&out).unwrap_or_default();
        if !t.lines().any(|l| l == "ready") {
            return false;
        }
        for l in t.lines() {
            if let Some(k) = l.strip_prefix("member ") {
                d.key = k.to_string();
            }
            if let Some(a) = l.strip_prefix("listen ") {
                d.addr = a.to_string();
            }
        }
        true
    });
    d
}

fn invite(d: &Daemon) -> String {
    let o = prim().arg("ctl").arg(format!("--socket={}", d.socket.display())).arg("invite").output().unwrap();
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

/// A founder and `n-1` joiners.
fn room(dir: &Path, n: usize) -> Vec<Daemon> {
    let mut v = vec![start_daemon(dir, "d0", None)];
    for i in 1..n {
        let tok = invite(&v[0]);
        let d = start_daemon(dir, &format!("d{i}"), Some((&tok, &v[..])));
        v.push(d);
    }
    // everyone sees everyone
    for d in &v {
        wait_for(30.0, "roster", || {
            let o = prim().arg("ctl").arg(format!("--socket={}", d.socket.display())).arg("status").output().unwrap();
            let t = String::from_utf8_lossy(&o.stdout);
            t.lines().filter(|l| l.starts_with("member ") && l.contains("online=true")).count() >= n
        });
    }
    v
}

// ---- players ----------------------------------------------------------------------------

struct Ipc {
    r: BufReader<UnixStream>,
    w: UnixStream,
    id: u64,
}

impl Ipc {
    fn connect(p: &Path) -> Ipc {
        wait_for(20.0, "mpv ipc", || p.exists() && UnixStream::connect(p).is_ok());
        let s = UnixStream::connect(p).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        Ipc { r: BufReader::new(s.try_clone().unwrap()), w: s, id: 1000 }
    }
    fn call(&mut self, cmd: Value) -> Option<Value> {
        self.id += 1;
        let line = format!("{}\n", json!({"command": cmd, "request_id": self.id}));
        self.w.write_all(line.as_bytes()).ok()?;
        loop {
            let mut l = String::new();
            self.r.read_line(&mut l).ok()?;
            let v: Value = serde_json::from_str(&l).ok()?;
            if v["request_id"] == self.id {
                return (v["error"] == "success").then(|| v["data"].clone());
            }
        }
    }
    fn get(&mut self, p: &str) -> Option<Value> {
        self.call(json!(["get_property", p]))
    }
}

struct Watcher {
    child: Child,
    ipc_path: PathBuf,
    ipc: Ipc,
    log: PathBuf,
    started: Instant,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.ipc_path);
    }
}

fn watch(dir: &Path, d: &Daemon, name: &str, source: Option<&str>, extra_env: &[(&str, &str)]) -> Watcher {
    let ipc_path = dir.join(format!("{name}.mpv.sock"));
    let log = dir.join(format!("{name}.log"));
    let mut c = prim();
    c.arg("watch")
        .arg(format!("--socket={}", d.socket.display()))
        .arg(format!("--ipc={}", ipc_path.display()))
        .env("PRIM_WATCH_MPV_ARGS", "--vo=null --ao=null")
        .env("PRIM_WATCH_DEBUG", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log).unwrap());
    for (k, v) in extra_env {
        c.env(k, v);
    }
    if let Some(s) = source {
        c.arg(s);
    }
    let started = Instant::now();
    let child = c.spawn().unwrap();
    let ipc = Ipc::connect(&ipc_path);
    Watcher { child, ipc_path, ipc, log, started }
}

impl Watcher {
    fn pos(&mut self) -> Option<(f64, bool)> {
        let p = self.ipc.get("time-pos")?.as_f64()?;
        let paused = self.ipc.get("pause")?.as_bool()?;
        Some((p, paused))
    }
    fn log_text(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
    fn seeks(&self) -> usize {
        self.log_text().matches("player Seek(").count()
    }
}

/// Position difference between two players, with the read-time gap removed when both play.
fn skew(a: &mut Watcher, b: &mut Watcher) -> Option<f64> {
    let t0 = Instant::now();
    let (pa, qa) = a.pos()?;
    let t1 = Instant::now();
    let (pb, qb) = b.pos()?;
    let dt = if !qa && !qb { t1.duration_since(t0).as_secs_f64() } else { 0.0 };
    // b was read dt later than a
    Some((pa - (pb - dt)).abs())
}

fn settle(secs: f64, what: &str, a: &mut Watcher, b: &mut Watcher, bound: f64) -> f64 {
    wait_for(secs, what, || skew(a, b).is_some_and(|s| s < bound))
}

// ---- tests ------------------------------------------------------------------------------

/// Load, play, pause, seek from either side, and a late joiner, with the drift bound the
/// crate claims (0.15 s steady; the bound used here adds slack for read jitter).
#[test]
fn load_pause_seek_and_late_join() {
    need_tools!();
    let dir = testdir();
    let media = clip(dir.path(), "t60.mp4", 60);
    let ds = room(dir.path(), 3);
    eprintln!("load average at start: {:.1}", load_factor());
    let bound = 0.25 + 0.1 * (load_factor() / 8.0).min(3.0);

    let mut a = watch(dir.path(), &ds[0], "a", Some(media.to_str().unwrap()), &[]);
    let mut b = watch(dir.path(), &ds[1], "b", None, &[]);
    // b joins what a loaded, without being told anything
    wait_for(30.0, "b playing the same file", || {
        a.pos().is_some_and(|(_, p)| !p) && b.pos().is_some_and(|(_, p)| !p)
    });
    let t = settle(20.0, "a and b together", &mut a, &mut b, bound);
    eprintln!("converged {t:.2}s after both playing");

    // steady state: 8 s of samples
    let mut worst: f64 = 0.0;
    let end = Instant::now() + Duration::from_secs(8);
    while Instant::now() < end {
        if let Some(s) = skew(&mut a, &mut b) {
            worst = worst.max(s);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    eprintln!("steady worst skew {worst:.3}s (bound {bound:.2})");
    assert!(worst < bound, "steady skew {worst}");

    // pause from b moves a
    b.ipc.call(json!(["set_property", "pause", true]));
    wait_for(5.0, "a paused by b", || a.pos().is_some_and(|(_, p)| p) && b.pos().is_some_and(|(_, p)| p));
    let s = skew(&mut a, &mut b).unwrap();
    assert!(s < 0.1, "paused at different spots: {s}");

    // seek from a moves b (while paused)
    a.ipc.call(json!(["seek", 40.0, "absolute"]));
    wait_for(8.0, "b at 40 s", || b.pos().is_some_and(|(p, _)| (p - 40.0).abs() < 0.3) && a.pos().is_some_and(|(p, _)| (p - 40.0).abs() < 0.3));

    // play from a moves b
    a.ipc.call(json!(["set_property", "pause", false]));
    wait_for(8.0, "both playing", || a.pos().is_some_and(|(_, p)| !p) && b.pos().is_some_and(|(_, p)| !p));
    settle(10.0, "a and b together after play", &mut a, &mut b, bound);

    // seek from b moves a (while playing); b, who did it, is not seeked again
    let (a0, b0) = (a.seeks(), b.seeks());
    b.ipc.call(json!(["seek", 10.0, "absolute"]));
    let t = wait_for(8.0, "a followed b's seek", || a.pos().is_some_and(|(p, _)| (p - 10.0).abs() < 1.5) && skew(&mut a, &mut b).is_some_and(|s| s < bound));
    eprintln!("seek from b reached a and settled in {t:.2}s");
    std::thread::sleep(Duration::from_secs(2));
    eprintln!("seeks caused by it: a +{} b +{}", a.seeks() - a0, b.seeks() - b0);
    assert_eq!(b.seeks() - b0, 0, "the player that was seeked by its person was seeked again");
    assert!(a.seeks() - a0 <= 2, "seek storm on the follower");

    // late joiner
    std::thread::sleep(Duration::from_secs(3));
    let mut c = watch(dir.path(), &ds[2], "c", None, &[]);
    let mut first_playing = None;
    let t = wait_for(30.0, "c joined", || {
        let playing = c.pos().is_some_and(|(_, p)| !p);
        if playing && first_playing.is_none() {
            first_playing = Some(c.started.elapsed().as_secs_f64());
        }
        playing && skew(&mut a, &mut c).is_some_and(|s| s < 0.75)
    });
    eprintln!("late joiner within 0.75 s of a {t:.2}s after its process started (first playing at {first_playing:?})");
    settle(10.0, "c tight with a", &mut a, &mut c, bound);
    // no seek storm: a handful of seeks per player for everything above
    eprintln!("seeks issued: a={} b={} c={}", a.seeks(), b.seeks(), c.seeks());
    assert!(a.seeks() + b.seeks() + c.seeks() < 30, "seek storm");
}

/// A clean file reaches the end and says Finished, without reloading.
#[test]
fn clean_end_is_finished() {
    need_tools!();
    let dir = testdir();
    let media = clip(dir.path(), "t6.mp4", 6);
    let ds = room(dir.path(), 2);
    let mut a = watch(dir.path(), &ds[0], "a", Some(media.to_str().unwrap()), &[]);
    let mut b = watch(dir.path(), &ds[1], "b", None, &[]);
    let t = wait_for(40.0, "both Finished", || {
        a.log_text().contains("notice Finished") && b.log_text().contains("notice Finished")
    });
    eprintln!("both Finished {t:.1}s after start (6 s clip)");
    assert!(!a.log_text().contains("Reloading") && !b.log_text().contains("Reloading"));
}

/// HTTP server that serves `file` but drops the first `cut` connections after `after`
/// body bytes. Returns the url, a counter of connections, and a stop flag.
fn flaky_server(file: PathBuf, after: usize, cut: usize) -> (String, Arc<AtomicUsize>, Arc<AtomicBool>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/clip.mp4", l.local_addr().unwrap());
    let conns = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let (c2, s2) = (conns.clone(), stop.clone());
    l.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        let data = std::fs::read(&file).unwrap();
        while !s2.load(Ordering::Relaxed) {
            let Ok((mut s, _)) = l.accept() else {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            };
            s.set_nonblocking(false).unwrap();
            let n = c2.fetch_add(1, Ordering::Relaxed);
            let data = data.clone();
            std::thread::spawn(move || {
                let mut req = Vec::new();
                let mut buf = [0u8; 1024];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(k) => req.extend_from_slice(&buf[..k]),
                    }
                }
                let req = String::from_utf8_lossy(&req).to_lowercase();
                let start = req
                    .lines()
                    .find_map(|l| l.strip_prefix("range: bytes="))
                    .and_then(|r| r.split('-').next()?.trim().parse::<usize>().ok())
                    .unwrap_or(0)
                    .min(data.len());
                let body = &data[start..];
                eprintln!("  http conn {n}: {} start={start} len={}", req.lines().next().unwrap_or(""), body.len());
                let head = if start > 0 {
                    format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{}/{}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nContent-Type: video/mp4\r\nConnection: close\r\n\r\n",
                        data.len() - 1,
                        data.len(),
                        body.len()
                    )
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nContent-Type: video/mp4\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                };
                let _ = s.write_all(head.as_bytes());
                if req.starts_with("head") {
                    return;
                }
                if n < cut {
                    // the connection dies partway through the promised body
                    let k = after.saturating_sub(start).min(body.len());
                    let _ = s.write_all(&body[..k]);
                    let _ = s.flush();
                    std::thread::sleep(Duration::from_millis(300));
                    return; // drops the socket mid-body
                }
                let _ = s.write_all(body);
            });
        }
    });
    (url, conns, stop)
}

/// A stream that dies partway is not "Finished": it reloads at the room position and
/// carries on to a real end.
#[test]
fn dropped_stream_reloads_and_does_not_finish() {
    need_tools!();
    let dir = testdir();
    let media = clip(dir.path(), "t20.mp4", 20);
    let size = std::fs::metadata(&media).unwrap().len() as usize;
    // The first six connections die at ~45% of the file: mpv's own reconnects (three) are used up, then the player hits end of stream early
    let (url, conns, stop) = flaky_server(media, size * 45 / 100, 6);
    let ds = room(dir.path(), 1);
    let mut a = watch(dir.path(), &ds[0], "a", Some(&url), &[]);
    let t = wait_for(40.0, "a reload notice", || a.log_text().contains("Reloading"));
    eprintln!("Reloading notice {t:.1}s after start; server saw {} connections", conns.load(Ordering::Relaxed));
    assert!(!a.log_text().contains("notice Finished"), "finished on a dropped stream");
    // after the reload it gets past the cut and ends cleanly
    wait_for(90.0, "Finished after reload", || a.log_text().contains("notice Finished"));
    let (p, _) = a.pos().unwrap();
    eprintln!("finished at {p:.1}s; connections {}", conns.load(Ordering::Relaxed));
    assert!(p > 15.0, "finished at {p}");
    stop.store(true, Ordering::Relaxed);
}

/// A stall the player does not report (the machine's correction has to find it):
/// b's mpv is stopped for half a second (0.5 s behind), and the room pulls it back. With the
/// correction off (the fault hook) the offset stays.
#[test]
fn hidden_half_second_offset_is_corrected() {
    need_tools!();
    for correction_off in [false, true] {
        let dir = testdir();
        let media = clip(dir.path(), "t60.mp4", 60);
        let ds = room(dir.path(), 2);
        let env: &[(&str, &str)] = if correction_off { &[("PRIM_WATCH_FAULT", "no_correction")] } else { &[] };
        let mut a = watch(dir.path(), &ds[0], "a", Some(media.to_str().unwrap()), &[]);
        let mut b = watch(dir.path(), &ds[1], "b", None, env);
        wait_for(30.0, "together", || skew(&mut a, &mut b).is_some_and(|s| s < 0.3) && a.pos().is_some_and(|(_, p)| !p));
        std::thread::sleep(Duration::from_secs(4));
        // b's mpv stalls for half a second (a hiccup nobody reported)
        let pid = b.ipc.get("pid").and_then(|v| v.as_i64()).unwrap() as i32;
        unsafe { libc::kill(pid, libc::SIGSTOP) };
        std::thread::sleep(Duration::from_millis(500));
        unsafe { libc::kill(pid, libc::SIGCONT) };
        let injected = skew(&mut a, &mut b).unwrap();
        eprintln!("correction_off={correction_off}: injected offset {injected:.2}s");
        assert!(injected > 0.35, "fault did not inject: {injected}");
        if correction_off {
            std::thread::sleep(Duration::from_secs(6));
            let s = skew(&mut a, &mut b).unwrap();
            eprintln!("  after 6 s with correction off: {s:.2}s");
            assert!(s > 0.35, "offset vanished with correction off: {s}");
        } else {
            let t = wait_for(15.0, "offset corrected", || skew(&mut a, &mut b).is_some_and(|s| s < 0.2));
            eprintln!("  corrected in {t:.1}s");
        }
    }
}

/// The founder's daemon is the coordinator but runs no player: the others still control
/// each other.
#[test]
fn coordinator_daemon_without_player() {
    need_tools!();
    let dir = testdir();
    let media = clip(dir.path(), "t60.mp4", 60);
    let ds = room(dir.path(), 3);
    let mut b = watch(dir.path(), &ds[1], "b", Some(media.to_str().unwrap()), &[]);
    let mut c = watch(dir.path(), &ds[2], "c", None, &[]);
    wait_for(60.0, "b and c playing", || {
        b.pos().is_some_and(|(_, p)| !p) && c.pos().is_some_and(|(_, p)| !p)
    });
    settle(20.0, "together", &mut b, &mut c, 0.3);
    c.ipc.call(json!(["set_property", "pause", true]));
    let t = wait_for(15.0, "b paused by c", || b.pos().is_some_and(|(_, p)| p));
    eprintln!("pause from c reached b in {t:.1}s with the coordinator daemon having no player");
    b.ipc.call(json!(["seek", 30.0, "absolute"]));
    wait_for(15.0, "c at 30", || c.pos().is_some_and(|(p, _)| (p - 30.0).abs() < 0.3));
}

/// One YouTube URL through mpv's yt-dlp path. Network and yt-dlp needed:
/// `PRIM_WATCH_YT=https://www.youtube.com/watch?v=... cargo test -p prim --test watch -- --ignored youtube`
#[test]
#[ignore]
fn youtube_smoke() {
    need_tools!();
    let Ok(url) = std::env::var("PRIM_WATCH_YT") else {
        eprintln!("SKIP: set PRIM_WATCH_YT");
        return;
    };
    let dir = testdir();
    let ds = room(dir.path(), 2);
    let mut a = watch(dir.path(), &ds[0], "a", Some(&url), &[]);
    let mut b = watch(dir.path(), &ds[1], "b", None, &[]);
    wait_for(120.0, "both playing", || a.pos().is_some_and(|(_, p)| !p) && b.pos().is_some_and(|(_, p)| !p));
    settle(30.0, "together", &mut a, &mut b, 0.4);
    std::thread::sleep(Duration::from_secs(6));
    let s = skew(&mut a, &mut b).unwrap();
    eprintln!("youtube: skew {s:.2}s, a at {:?}", a.pos());
    assert!(s < 0.5);
}

