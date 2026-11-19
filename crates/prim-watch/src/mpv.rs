//! A child `mpv` and its JSON IPC (`--input-ipc-server`): one reader thread parses
//! lines into a channel; the owner issues commands and polls events from one thread.
//! <https://mpv.io/manual/master/#json-ipc>

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub enum MpvEvent {
    FileLoaded,
    EndFile { reason: String },
    Seek,
    PlaybackRestart,
    Prop { name: String, data: Value },
    /// The IPC connection closed: mpv exited.
    Gone,
}

pub struct Mpv {
    child: Child,
    w: UnixStream,
    rx: Receiver<Value>,
    pending: VecDeque<Value>,
    next_id: u64,
    pub sock: PathBuf,
    own_sock: bool,
    gone: bool,
}

impl Mpv {
    /// Start mpv idle (window open, nothing loaded) with an IPC socket at `sock`.
    pub fn spawn(exe: &str, sock: &Path, extra: &[String]) -> Result<Mpv> {
        let _ = std::fs::remove_file(sock);
        let mut c = Command::new(exe);
        c.args([
            "--idle=yes",
            "--force-window=yes",
            // Stay on the last frame at the end, so "finished" and "dropped early" are
            // both visible as a loaded file at end of file.
            "--keep-open=yes",
            "--pause",
            "--no-resume-playback",
            "--save-position-on-quit=no",
            "--loop-file=no",
            "--loop-playlist=no",
            "--title=Prim: watch together",
            "--msg-level=all=warn",
        ])
        .arg(format!("--input-ipc-server={}", sock.display()))
        .args(extra)
        .stdin(Stdio::null());
        // SAFETY: only async-signal-safe prctl between fork and exec.
        unsafe {
            c.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
        let child = c
            .spawn()
            .with_context(|| format!("could not start {exe}: is mpv installed? (or set PRIM_MPV)"))?;
        let mut me_child = child;
        let deadline = Instant::now() + Duration::from_secs(15);
        let stream = loop {
            match UnixStream::connect(sock) {
                Ok(s) => break s,
                Err(e) => {
                    if let Ok(Some(st)) = me_child.try_wait() {
                        bail!("mpv exited at start ({st})");
                    }
                    if Instant::now() > deadline {
                        let _ = me_child.kill();
                        let _ = me_child.wait();
                        bail!("mpv's IPC socket {} never appeared ({e})", sock.display());
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        };
        let r = stream.try_clone()?;
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("mpv-ipc".into())
            .spawn(move || {
                for line in BufReader::new(r).lines() {
                    let Ok(line) = line else { break };
                    if let Ok(v) = serde_json::from_str::<Value>(&line) {
                        if tx.send(v).is_err() {
                            return;
                        }
                    }
                }
                let _ = tx.send(json!({"event":"prim-gone"}));
            })?;
        Ok(Mpv {
            child: me_child,
            w: stream,
            rx,
            pending: VecDeque::new(),
            next_id: 1,
            sock: sock.to_path_buf(),
            own_sock: true,
            gone: false,
        })
    }

    fn write(&mut self, cmd: &[Value], id: u64) -> Result<()> {
        let mut line = serde_json::to_vec(&json!({"command": cmd, "request_id": id}))?;
        line.push(b'\n');
        self.w.write_all(&line).context("mpv IPC write")?;
        Ok(())
    }

    /// Issue a command and do not wait. Failures show up in `poll`'s log of errors.
    pub fn fire(&mut self, cmd: &[Value]) {
        let id = self.next_id;
        self.next_id += 1;
        if self.write(cmd, id).is_err() {
            self.gone = true;
        }
    }

    /// Issue a command and wait for its reply data. Events that arrive meanwhile are kept.
    pub fn call(&mut self, cmd: &[Value]) -> Option<Value> {
        let id = self.next_id;
        self.next_id += 1;
        if self.write(cmd, id).is_err() {
            self.gone = true;
            return None;
        }
        let deadline = Instant::now() + Duration::from_millis(1500);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(v) => {
                    if v.get("request_id").and_then(Value::as_u64) == Some(id) {
                        return (v.get("error").and_then(Value::as_str) == Some("success"))
                            .then(|| v.get("data").cloned().unwrap_or(Value::Null));
                    }
                    self.pending.push_back(v);
                }
                Err(RecvTimeoutError::Timeout) => return None,
                Err(RecvTimeoutError::Disconnected) => {
                    self.gone = true;
                    return None;
                }
            }
        }
    }

    pub fn get(&mut self, prop: &str) -> Option<Value> {
        self.call(&[json!("get_property"), json!(prop)])
    }

    pub fn get_f64(&mut self, prop: &str) -> Option<f64> {
        self.get(prop).and_then(|v| v.as_f64())
    }

    pub fn set(&mut self, prop: &str, v: Value) {
        self.fire(&[json!("set_property"), json!(prop), v]);
    }

    pub fn observe(&mut self, id: u64, prop: &str) {
        self.fire(&[json!("observe_property"), json!(id), json!(prop)]);
    }

    pub fn show_text(&mut self, text: &str, ms: u64) {
        self.fire(&[json!("show-text"), json!(text), json!(ms)]);
    }

    /// Everything that happened since the last call.
    pub fn poll(&mut self) -> Vec<MpvEvent> {
        let mut raw: Vec<Value> = self.pending.drain(..).collect();
        loop {
            match self.rx.try_recv() {
                Ok(v) => raw.push(v),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.gone = true;
                    break;
                }
            }
        }
        let mut out = vec![];
        for v in raw {
            if let Some(err) = v.get("error").and_then(Value::as_str) {
                if err != "success" && v.get("event").is_none() {
                    eprintln!("prim watch: mpv command failed: {err}");
                }
                continue;
            }
            let Some(ev) = v.get("event").and_then(Value::as_str) else {
                continue;
            };
            match ev {
                "file-loaded" => out.push(MpvEvent::FileLoaded),
                "end-file" => out.push(MpvEvent::EndFile {
                    reason: v.get("reason").and_then(Value::as_str).unwrap_or("").into(),
                }),
                "seek" => out.push(MpvEvent::Seek),
                "playback-restart" => out.push(MpvEvent::PlaybackRestart),
                "property-change" => {
                    if let Some(name) = v.get("name").and_then(Value::as_str) {
                        out.push(MpvEvent::Prop {
                            name: name.into(),
                            data: v.get("data").cloned().unwrap_or(Value::Null),
                        });
                    }
                }
                "shutdown" | "prim-gone" => {
                    self.gone = true;
                }
                _ => {}
            }
        }
        if self.gone {
            out.push(MpvEvent::Gone);
        }
        out
    }

    pub fn exited(&mut self) -> bool {
        self.gone || matches!(self.child.try_wait(), Ok(Some(_)))
    }

    pub fn quit(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if self.own_sock {
            let _ = std::fs::remove_file(&self.sock);
        }
    }
}

impl Drop for Mpv {
    fn drop(&mut self) {
        self.quit();
    }
}
