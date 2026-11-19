//! Maps mpv onto the cowatch player interface: `PlayerCmd` in, `PlayerReport` and user
//! commands out. The hard part is telling what the person did in mpv's window (keys,
//! OSC) from what we did ourselves: every command we send leaves an expectation, and
//! only property changes and seeks that match no expectation are the person's.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use prim_cowatch::wire::SourceKind;
use prim_cowatch::{PlayerCmd, PlayerReport, PlayerState, UserCommand};
use serde_json::json;

use crate::mpv::{Mpv, MpvEvent};

const EXPECT_TTL: Duration = Duration::from_secs(3);

pub struct Player {
    pub mpv: Mpv,
    token: u32,
    has_src: bool,
    /// loadfile sent, file-loaded not yet seen.
    load_pending: bool,
    errored: bool,
    eof: bool,
    seeking_prop: bool,
    buffering: bool,
    /// Our seeks mpv has not yet finished (playback-restart clears).
    seek_inflight: u32,
    /// Seek events we expect and must not report as the person's.
    own_seeks: VecDeque<Instant>,
    /// Pause values we set and expect to see come back as property changes.
    expect_pause: VecDeque<(bool, Instant)>,
    pause: bool,
    /// mpv went paused and the person may have done it: judged once mpv has had a moment
    /// to say it reached the end of the stream (it pauses itself there).
    maybe_user_pause: Option<Instant>,
    /// A load with a start position makes mpv seek once after it loads.
    expect_start_seek: bool,
    duration: f64,
    last_pos: f64,
}

impl Player {
    pub fn new(mut mpv: Mpv) -> Player {
        for (i, p) in ["pause", "eof-reached", "seeking", "paused-for-cache", "duration"]
            .iter()
            .enumerate()
        {
            mpv.observe(i as u64 + 1, p);
        }
        Player {
            mpv,
            token: 0,
            has_src: false,
            load_pending: false,
            errored: false,
            eof: false,
            seeking_prop: false,
            buffering: false,
            seek_inflight: 0,
            own_seeks: VecDeque::new(),
            expect_pause: VecDeque::new(),
            pause: true,
            maybe_user_pause: None,
            expect_start_seek: false,
            duration: 0.0,
            last_pos: 0.0,
        }
    }

    pub fn has_src(&self) -> bool {
        self.has_src
    }

    pub fn apply(&mut self, cmd: &PlayerCmd) {
        match cmd {
            PlayerCmd::Load {
                token,
                kind,
                source,
                start_s,
            } => {
                let src = String::from_utf8_lossy(source).into_owned();
                self.token = *token;
                self.has_src = true;
                self.load_pending = true;
                self.errored = false;
                self.eof = false;
                self.seek_inflight = 0;
                self.buffering = false;
                self.duration = 0.0;
                self.own_seeks.clear();
                self.expect_pause.clear();
                self.pause = true;
                self.maybe_user_pause = None;
                self.expect_start_seek = *start_s > 0.0;
                let _ = kind; // URLs and file paths are both just mpv "files"
                let mut opts = String::from("pause=yes");
                if *start_s > 0.0 {
                    opts.push_str(&format!(",start={start_s:.3}"));
                }
                self.mpv.set("speed", json!(1.0));
                self.mpv
                    .fire(&[json!("loadfile"), json!(src), json!("replace"), json!(-1), json!(opts)]);
            }
            PlayerCmd::Seek(t) => {
                if self.load_pending || !self.has_src {
                    return;
                }
                self.seek_inflight += 1;
                self.own_seeks.push_back(Instant::now());
                self.last_pos = *t;
                self.mpv
                    .fire(&[json!("seek"), json!(t), json!("absolute+exact")]);
            }
            PlayerCmd::SetPaused(p) => {
                if self.pause != *p {
                    self.expect_pause.push_back((*p, Instant::now()));
                    self.pause = *p;
                    self.mpv.set("pause", json!(p));
                }
            }
            PlayerCmd::SetSpeed(s) => self.mpv.set("speed", json!(s)),
            PlayerCmd::Stop => {
                self.has_src = false;
                self.load_pending = false;
                self.mpv.fire(&[json!("stop")]);
            }
        }
    }

    fn at_end(&self) -> bool {
        self.eof || (self.duration > 0.0 && self.last_pos >= self.duration - 0.3)
    }

    /// Absorb mpv's events. Returns the person's commands and whether mpv is gone.
    pub fn poll(&mut self) -> (Vec<UserCommand>, bool) {
        let now = Instant::now();
        self.own_seeks.retain(|t| now.duration_since(*t) < EXPECT_TTL);
        self.expect_pause.retain(|(_, t)| now.duration_since(*t) < EXPECT_TTL);
        let mut cmds = vec![];
        let mut gone = false;
        for ev in self.mpv.poll() {
            match ev {
                MpvEvent::Gone => gone = true,
                MpvEvent::FileLoaded => {
                    self.load_pending = false;
                    self.errored = false;
                    if let Some(p) = self.mpv.get("pause").and_then(|v| v.as_bool()) {
                        self.pause = p;
                    }
                    if let Some(d) = self.mpv.get_f64("duration") {
                        self.duration = d;
                    }
                }
                MpvEvent::EndFile { reason } => match reason.as_str() {
                    "error" | "unknown" => {
                        self.errored = true;
                        self.load_pending = false;
                    }
                    "eof" => self.eof = true,
                    _ => {}
                },
                MpvEvent::Seek => {
                    if std::mem::take(&mut self.expect_start_seek) {
                        // mpv's own seek to the `start` we gave it.
                    } else if self.own_seeks.pop_front().is_none() && self.has_src && !self.load_pending {
                        // The person seeked in mpv: the target is already time-pos.
                        if let Some(t) = self.mpv.get_f64("time-pos") {
                            // Until mpv finishes the seek, report it as seeking at its
                            // target, or the machine sees a player far from the room and
                            // seeks the person back.
                            self.seek_inflight += 1;
                            self.last_pos = t;
                            self.eof = false;
                            cmds.push(UserCommand::SeekTo(t));
                        }
                    }
                }
                MpvEvent::PlaybackRestart => self.seek_inflight = 0,
                MpvEvent::Prop { name, data } => match name.as_str() {
                    "pause" => {
                        let Some(v) = data.as_bool() else { continue };
                        if self.expect_pause.front().map(|e| e.0) == Some(v) {
                            self.expect_pause.pop_front();
                            self.pause = v;
                        } else if v != self.pause {
                            self.pause = v;
                            // mpv pauses itself at the end of the stream (keep-open), also
                            // an early one, and while a file loads, and refuses to play from
                            // the end; none of that is the person asking the room for
                            // anything.
                            if self.has_src && !self.load_pending && !self.at_end() {
                                if v {
                                    self.maybe_user_pause = Some(now);
                                } else {
                                    self.maybe_user_pause = None;
                                    cmds.push(UserCommand::Play);
                                }
                            }
                        }
                    }
                    "eof-reached" => self.eof = data.as_bool().unwrap_or(false),
                    "seeking" => self.seeking_prop = data.as_bool().unwrap_or(false),
                    "paused-for-cache" => self.buffering = data.as_bool().unwrap_or(false),
                    "duration" => self.duration = data.as_f64().unwrap_or(0.0),
                    _ => {}
                },
            }
        }
        if let Some(t) = self.maybe_user_pause {
            if self.eof || self.errored || self.load_pending {
                self.maybe_user_pause = None;
            } else if now.duration_since(t) >= Duration::from_millis(250) {
                self.maybe_user_pause = None;
                if self.pause && self.has_src && !self.at_end() {
                    cmds.push(UserCommand::Pause);
                }
            }
        }
        if self.mpv.exited() {
            gone = true;
        }
        (cmds, gone)
    }

    pub fn report(&mut self) -> PlayerReport {
        if let Some(t) = self.mpv.get_f64("time-pos") {
            if self.seek_inflight == 0 {
                self.last_pos = t;
            }
        }
        let state = if self.load_pending {
            PlayerState::Loading
        } else if self.errored {
            PlayerState::Error
        } else if self.eof {
            PlayerState::Ended
        } else if self.seek_inflight > 0 || self.seeking_prop {
            PlayerState::Seeking
        } else if self.buffering {
            PlayerState::Buffering
        } else {
            PlayerState::Ready
        };
        PlayerReport {
            token: self.token,
            state,
            position_s: self.last_pos,
            duration_s: if self.load_pending { 0.0 } else { self.duration },
            paused: self.pause,
        }
    }
}

pub fn kind_of(source: &str) -> SourceKind {
    if source.contains("://") {
        SourceKind::Url
    } else {
        SourceKind::File
    }
}

