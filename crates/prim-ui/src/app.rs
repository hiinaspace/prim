//! The window (egui). Reads the [`Model`] every frame; everything it does goes through
//! [`Handle`] commands, [`supervise`] or a short background thread.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{
    self, Align, Color32, FontId, Layout, RichText, Sense, Stroke, StrokeKind, Ui, Vec2,
};

use crate::chat::{self, Clients};
use crate::client::{self, AttachOpts, Delivery, Handle, MemberRow, Model, Presence};
use crate::icons::{self, Icon};
use crate::supervise::{self, ProcState, StartMode, Voice};

// ---- the launch table ---------------------------------------------------------------
//
// Adding a client later is one entry here: its label, what it is, and either
// `unavailable` (with the reason shown to the user) or the program to run.

pub enum ClientKind {
    /// `prim voice`, with the mic and deafen controls.
    Voice,
    /// `prim watch`, with a link or file entry.
    Watch,
    /// A program started with `argv` (the `prim` binary itself is `argv[0] == "prim"`).
    Program(&'static [&'static str]),
}

pub struct ClientSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub blurb: &'static str,
    pub icon: Icon,
    pub kind: ClientKind,
    pub unavailable: Option<&'static str>,
}

pub const CLIENTS: &[ClientSpec] = &[
    ClientSpec {
        id: "voice",
        label: "Voice chat",
        blurb: "Talk and listen to everyone in the room",
        icon: Icon::Headphones,
        kind: ClientKind::Voice,
        unavailable: None,
    },
    ClientSpec {
        id: "godot",
        label: "3D room",
        blurb: "Walk around the living room as an avatar",
        icon: Icon::Cube,
        kind: ClientKind::Program(&[]),
        unavailable: Some("Not ready yet (3D client round C1)."),
    },
    ClientSpec {
        id: "player",
        label: "Watch together (mpv)",
        blurb: "Play a video in sync with the room",
        icon: Icon::Play,
        kind: ClientKind::Watch,
        unavailable: None,
    },
];

// ---- theme ----------------------------------------------------------------------------

const BG: Color32 = Color32::from_rgb(22, 24, 29);
const PANEL: Color32 = Color32::from_rgb(30, 33, 40);
const PANEL2: Color32 = Color32::from_rgb(37, 41, 50);
const LINE: Color32 = Color32::from_rgb(52, 57, 68);
const TEXT: Color32 = Color32::from_rgb(222, 226, 232);
const DIM: Color32 = Color32::from_rgb(138, 146, 160);
const GREEN: Color32 = Color32::from_rgb(88, 200, 130);
const AMBER: Color32 = Color32::from_rgb(232, 178, 70);
const RED: Color32 = Color32::from_rgb(235, 98, 88);
const ACCENT: Color32 = Color32::from_rgb(84, 170, 235);

fn style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = PANEL;
    v.extreme_bg_color = Color32::from_rgb(16, 18, 22);
    v.override_text_color = Some(TEXT);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    v.widgets.inactive.bg_fill = PANEL2;
    v.widgets.inactive.weak_bg_fill = PANEL2;
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(48, 54, 66);
    v.selection.bg_fill = Color32::from_rgb(40, 90, 140);
    ctx.set_visuals(v);
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = Vec2::new(8.0, 6.0);
        s.spacing.button_padding = Vec2::new(10.0, 5.0);
        s.interaction.tooltip_delay = 0.15;
    });
}

// ---- app --------------------------------------------------------------------------------

enum Confirm {
    Leave,
    Close,
    Quit,
}

pub struct App {
    rt: tokio::runtime::Handle,
    /// `PRIM_UI_DEMO=voice,mic,deaf`: press those buttons once in a room (screenshot runs
    /// on a display with no pointer). Consumed on first use.
    demo: Option<String>,
    /// `PRIM_UI_ACT=create;join=<token>;say=<text>`: the same actions as the buttons, in order
    /// (an end-to-end run on a display with no pointer).
    act: Vec<String>,
    /// A voice client left over from an earlier window is taken over once, muted.
    adopted: bool,
    socket: PathBuf,
    handle: Option<Handle>,
    attaching: Option<Receiver<std::io::Result<Handle>>>,
    last_probe: Instant,
    /// A background daemon start or stop in progress, with what the user sees.
    busy: Option<(String, Receiver<Result<(), String>>)>,
    voice: Voice,
    /// `prim watch` (mpv), and the link or file typed for it.
    watch: supervise::Supervised,
    watch_url: String,
    audio: crate::audio::Panel,
    audio_test_pending: Option<Instant>,
    sent_clients: Option<Clients>,
    name_edit: String,
    join_text: String,
    editing_name: Option<String>,
    /// The rename box takes focus once, on the frame it opens; asking every frame would
    /// swallow the Enter that ends the edit (and keep focus away from the chat box).
    editing_name_focus: bool,
    chat_input: String,
    advanced: bool,
    invite: Option<(u64, String)>,
    seen_invite: u64,
    error: Option<String>,
    confirm: Option<Confirm>,
    rooms: Vec<(String, SystemTime)>,
    quit_after_busy: bool,
    ctx_repaint: Option<egui::Context>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, rt: tokio::runtime::Handle) -> Self {
        style(&cc.egui_ctx);
        let socket = supervise::socket_path();
        let default_name = hostname();
        App {
            rt,
            demo: std::env::var("PRIM_UI_DEMO").ok(),
            act: std::env::var("PRIM_UI_ACT").map(|v| v.split(';').map(str::to_string).collect()).unwrap_or_default(),
            adopted: false,
            voice: Voice::new(&socket),
            watch: supervise::Supervised::new("watch"),
            watch_url: String::new(),
            audio_test_pending: None,
            audio: crate::audio::Panel::new(&socket, std::env::var_os("PRIM_UI_VOICE_TEST").is_some()),
            socket,
            handle: None,
            attaching: None,
            last_probe: Instant::now() - Duration::from_secs(10),
            busy: None,
            sent_clients: None,
            name_edit: default_name,
            join_text: String::new(),
            editing_name: None,
            editing_name_focus: false,
            chat_input: String::new(),
            advanced: std::env::var_os("PRIM_UI_ADVANCED").is_some(),
            invite: None,
            seen_invite: 0,
            error: None,
            confirm: None,
            rooms: supervise::saved_rooms(),
            quit_after_busy: false,
            ctx_repaint: Some(cc.egui_ctx.clone()),
        }
    }

    fn repaint_cb(&self) -> Arc<dyn Fn() + Send + Sync> {
        let ctx = self.ctx_repaint.clone();
        Arc::new(move || {
            if let Some(c) = &ctx {
                c.request_repaint();
            }
        })
    }

    fn opts(&self) -> AttachOpts {
        AttachOpts {
            repaint: Some(self.repaint_cb()),
            default_name: Some(self.name_edit.trim().to_string()).filter(|s| !s.is_empty()),
            chat: true,
        }
    }

    fn start_room(&mut self, mode: StartMode, what: &str) {
        let socket = self.socket.clone();
        let (tx, rx) = channel();
        let cb = self.repaint_cb();
        std::thread::spawn(move || {
            let r = supervise::start_daemon(&mode, &socket).map_err(|e| format!("{e:#}"));
            let _ = tx.send(r);
            cb();
        });
        self.busy = Some((what.to_string(), rx));
        self.error = None;
    }

    /// Leave the room and stop the daemon (the local API has no shutdown, so the
    /// process is found and stopped), then optionally quit the window.
    fn leave_room(&mut self, then_quit: bool) {
        if let Some(h) = &self.handle {
            h.send(client::Cmd::Leave);
        }
        self.voice.stop();
        self.watch.stop();
        let socket = self.socket.clone();
        let (tx, rx) = channel();
        let cb = self.repaint_cb();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400)); // let the goodbye go out
            let stopped = supervise::stop_daemon(&socket);
            let _ = tx.send(if stopped { Ok(()) } else { Err("Prim did not stop; it is still running on this computer.".to_string()) });
            cb();
        });
        self.busy = Some(("Leaving the room…".into(), rx));
        self.quit_after_busy = then_quit;
        self.handle = None;
        self.sent_clients = None;
    }

    /// Start `prim watch` for this room (replacing a running one). Empty `url` joins
    /// whatever the room is already watching.
    fn start_watch(&mut self, url: String) {
        let mut args = vec!["watch".to_string(), format!("--socket={}", self.socket.display())];
        let url = url.trim();
        if !url.is_empty() {
            args.push(url.to_string());
        }
        if let Err(e) = self.watch.start(&args) {
            self.watch.state = ProcState::Failed(e.to_string());
        }
    }

    fn tick(&mut self, ctx: &egui::Context) {
        self.voice.tick();
        self.watch.poll(false);
        // The panel reads the level from voice when its microphone is live, else runs its own meter.
        let mic_live = self.voice.sup.running() && self.voice.mic;
        self.audio.tick(mic_live);
        if let Some(d) = self.demo.clone() {
            // Screenshot runs on a display with no pointer: `audio` opens the panel,
            // `audiotest` starts the loopback test.
            if d.contains("audio") && !self.audio.open {
                self.audio.open();
                self.demo = Some(d.replace("audiotest", "").replace("audio", ""));
                if d.contains("audiotest") {
                    self.audio_test_pending = Some(Instant::now() + Duration::from_millis(1500));
                }
            }
        }
        if self.audio_test_pending.is_some_and(|t| Instant::now() >= t) {
            self.audio_test_pending = None;
            self.audio.start_test();
        }
        if !self.adopted && self.handle.as_ref().is_some_and(|h| h.lock().in_room()) {
            self.adopted = true;
            if !self.voice.sup.running() && !supervise::find_procs("voice", "rt-socket", &self.socket).is_empty() {
                self.voice.restart_with(false, false);
            }
        }
        if self.handle.as_ref().is_some_and(|h| h.lock().in_room()) {
            if let Some(d) = self.demo.take() {
                if let Some(u) = d.split(',').find_map(|p| p.strip_prefix("watch=")) {
                    self.start_watch(u.to_string());
                }
                if d.contains("voice") {
                    self.voice.start(false);
                    let (mic, deaf) = (d.contains("mic"), d.contains("deaf"));
                    if mic || deaf {
                        self.voice.restart_with(mic, deaf);
                    }
                }
            }
        }
        if let Some(a) = self.act.first().cloned() {
            let in_room = self.handle.as_ref().is_some_and(|h| h.lock().in_room());
            let idle = self.busy.is_none() && self.attaching.is_none() && self.handle.is_none();
            if a == "create" && idle {
                let meta = self.name_edit.clone();
                self.start_room(StartMode::Found { meta }, "Creating your room…");
                self.act.remove(0);
            } else if let (Some(t), true) = (a.strip_prefix("join="), idle) {
                self.start_room(StartMode::Join(t.to_string()), "Joining the room…");
                self.act.remove(0);
            } else if let (Some(t), true) = (a.strip_prefix("say="), in_room) {
                if let Some(h) = &self.handle {
                    h.chat(t);
                }
                self.act.remove(0);
            }
        }
        if let Some((_, rx)) = &self.busy {
            match rx.try_recv() {
                Ok(r) => {
                    self.busy = None;
                    if let Err(e) = r {
                        self.error = Some(e);
                    }
                    self.rooms = supervise::saved_rooms();
                    self.last_probe = Instant::now() - Duration::from_secs(10);
                    if self.quit_after_busy {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(_) => self.busy = None,
            }
        }
        if let Some(rx) = &self.attaching {
            if let Ok(r) = rx.try_recv() {
                self.attaching = None;
                match r {
                    Ok(h) => self.handle = Some(h),
                    Err(e) => self.error = Some(format!("could not attach: {e}")),
                }
            }
        }
        if let Some(h) = &self.handle {
            if !h.lock().attached {
                self.handle = None;
                self.sent_clients = None;
            }
        }
        // Attach to a running daemon, or notice that there is none.
        if self.handle.is_none()
            && self.attaching.is_none()
            && self.busy.is_none()
            && self.last_probe.elapsed() > Duration::from_millis(800)
        {
            self.last_probe = Instant::now();
            if supervise::daemon_alive(&self.socket) {
                let (tx, rx) = channel();
                let socket = self.socket.clone();
                let opts = self.opts();
                self.rt.spawn(async move {
                    let _ = tx.send(client::attach(&socket, opts).await);
                });
                self.attaching = Some(rx);
            }
        }
        // Tell the room which of our programs are running.
        if let Some(h) = &self.handle {
            let want = Clients {
                godot: false,
                player: false,
                deaf: self.voice_running() && self.voice.deaf,
            };
            if self.sent_clients != Some(want) && h.lock().snapshot_done {
                h.send(client::Cmd::SetClients(want));
                self.sent_clients = Some(want);
            }
            let inv = h.lock().invite.clone();
            if let Some((seq, tok)) = inv {
                if seq != self.seen_invite {
                    self.seen_invite = seq;
                    ctx.copy_text(tok.clone());
                    self.invite = Some((seq, tok));
                }
            }
        }
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    fn voice_running(&self) -> bool {
        self.voice.sup.running()
            || self
                .handle
                .as_ref()
                .is_some_and(|h| h.lock().voice_lease_held())
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.tick(ctx);
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let ctx = &ctx;
        let model_snapshot = self.handle.as_ref().map(|h| {
            let m = h.lock();
            Snapshot::of(&m)
        });
        match &model_snapshot {
            Some(s) if s.in_room => self.room_ui(ui, s),
            _ => self.empty_ui(ui, model_snapshot.is_some()),
        }
        self.dialogs(ctx);
        if self.audio.open && crate::audio::show(ctx, &mut self.audio) && self.voice.sup.running() {
            // Voice reads its devices when it starts: replace it, as the mute button does.
            let (m, d) = (self.voice.mic, self.voice.deaf);
            self.voice.restart_with(m, d);
        }
    }
}

/// What a frame needs from the model, copied out so the lock is not held while drawing.
pub(crate) struct Snapshot {
    pub(crate) in_room: bool,
    pub(crate) me: String,
    pub(crate) room: String,
    pub(crate) lifecycle: String,
    pub(crate) generation: Option<u32>,
    pub(crate) members: Vec<MemberRow>,
    pub(crate) am_keyholder: bool,
    pub(crate) chat: Vec<client::ChatLine>,
    pub(crate) names: std::collections::BTreeMap<String, String>,
    pub(crate) status: Option<prim_room::local::StatusJson>,
    pub(crate) keys: Vec<(String, client::KeyEntry)>,
    pub(crate) notice: Option<String>,
    pub(crate) removed: bool,
}

impl Snapshot {
    fn of(m: &Model) -> Snapshot {
        let members = m.members();
        let me = m.me().unwrap_or("").to_string();
        let names = members.iter().map(|r| (r.key.clone(), r.name.clone())).collect();
        Snapshot {
            in_room: m.in_room(),
            am_keyholder: members.iter().any(|r| r.is_me && r.keyholder),
            room: m
                .status
                .as_ref()
                .and_then(|s| s.room.clone())
                .unwrap_or_default(),
            lifecycle: m.status.as_ref().map(|s| s.lifecycle.clone()).unwrap_or_default(),
            generation: m.status.as_ref().and_then(|s| s.generation),
            members,
            me,
            chat: m.chat.clone(),
            names,
            status: m.status.clone(),
            keys: m.keys.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            notice: m.notice.clone(),
            removed: m.removed,
        }
    }
    pub(crate) fn name(&self, key: &str) -> String {
        self.names.get(key).cloned().unwrap_or_else(|| client::short(key))
    }
}

fn hostname() -> String {
    if let Ok(n) = std::env::var("PRIM_UI_NAME") {
        return n;
    }
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "me".into())
}

fn ago(t: SystemTime) -> String {
    let s = t.elapsed().map(|d| d.as_secs()).unwrap_or(0);
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86399 => format!("{} h ago", s / 3600),
        _ => format!("{} d ago", s / 86400),
    }
}

fn name_color(key: &str) -> Color32 {
    let h = key.bytes().fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    let hue = (h % 360) as f32 / 360.0;
    egui::ecolor::Hsva::new(hue, 0.45, 0.95, 1.0).into()
}

// ---- widgets --------------------------------------------------------------------------------

/// A toolbar button: icon, label, optional "on" colour.
fn tool_button(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    on: Option<Color32>,
    enabled: bool,
    hint: &str,
) -> egui::Response {
    let font = FontId::proportional(14.0);
    let galley = ui.painter().layout_no_wrap(label.to_string(), font, TEXT);
    let h = 30.0;
    let w = 12.0 + 18.0 + 6.0 + galley.size().x + 12.0;
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(w, h),
        if enabled { Sense::click() } else { Sense::hover() },
    );
    let hovered = resp.hovered() && enabled;
    let base = match on {
        Some(c) => c.gamma_multiply(if hovered { 0.38 } else { 0.28 }),
        None if hovered => Color32::from_rgb(52, 58, 70),
        None => PANEL2,
    };
    let stroke = on.map(|c| c.gamma_multiply(0.8)).unwrap_or(LINE);
    ui.painter().rect(rect, 6.0, base, Stroke::new(1.0, stroke), StrokeKind::Inside);
    let fg = if !enabled {
        DIM.gamma_multiply(0.6)
    } else {
        on.unwrap_or(TEXT)
    };
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 12.0 + 9.0, rect.center().y),
        Vec2::splat(18.0),
    );
    icons::paint(ui.painter(), icon_rect, icon, fg);
    ui.painter().galley(
        egui::pos2(icon_rect.right() + 6.0, rect.center().y - galley.size().y / 2.0),
        galley,
        fg,
    );
    resp.on_hover_text(hint)
}

fn small_icon(ui: &mut Ui, icon: Icon, color: Color32, hint: &str) {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
    icons::paint(ui.painter(), rect, icon, color);
    resp.on_hover_text(hint);
}

fn pill(ui: &mut Ui, text: &str, color: Color32) {
    ui.label(
        RichText::new(text)
            .size(11.0)
            .color(color)
            .background_color(color.gamma_multiply(0.18)),
    );
}

fn card(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(PANEL)
        .stroke(Stroke::new(1.0, LINE))
        .corner_radius(8.0)
        .inner_margin(14.0)
        .show(ui, add);
}

// ---- screens --------------------------------------------------------------------------------

impl App {
    fn empty_ui(&mut self, root: &mut Ui, attached_without_room: bool) {
        egui::CentralPanel::default().show(root, |ui| {
            ui.add_space(26.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("Prim").size(34.0).strong());
                ui.label(
                    RichText::new("A private room for you and a few friends: voice, text and shared video.")
                        .color(DIM),
                );
            });
            ui.add_space(18.0);
            let busy = self.busy.as_ref().map(|(w, _)| w.clone());
            let can = busy.is_none();
            ui.vertical_centered(|ui| {
                ui.set_max_width(640.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Your name").color(DIM));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.name_edit)
                            .desired_width(200.0)
                            .char_limit(chat::MAX_NAME),
                    );
                    ui.label(RichText::new("what friends see in the list").color(DIM).size(12.0));
                });
                ui.add_space(10.0);
                ui.columns(2, |cols| {
                    card(&mut cols[0], |ui| {
                        ui.label(RichText::new("Start a new room").strong().size(16.0));
                        ui.label(RichText::new("You open it, then send friends an invite.").color(DIM));
                        ui.add_space(8.0);
                        let b = ui.add_enabled(
                            can,
                            egui::Button::new(RichText::new("Create room").strong())
                                .fill(Color32::from_rgb(40, 110, 170)),
                        );
                        if b.clicked() {
                            let meta = self.name_edit.clone();
                            self.start_room(StartMode::Found { meta }, "Creating your room…");
                        }
                    });
                    card(&mut cols[1], |ui| {
                        ui.label(RichText::new("Join a friend's room").strong().size(16.0));
                        ui.label(RichText::new("Paste the invite they sent you.").color(DIM));
                        ui.add_space(8.0);
                        ui.add_enabled(
                            can,
                            egui::TextEdit::singleline(&mut self.join_text)
                                .hint_text("prim1…")
                                .desired_width(f32::INFINITY),
                        );
                        let ok = can && self.join_text.trim().starts_with("prim1");
                        if ui
                            .add_enabled(ok, egui::Button::new(RichText::new("Join room").strong()))
                            .clicked()
                        {
                            let t = self.join_text.trim().to_string();
                            self.start_room(StartMode::Join(t), "Joining the room…");
                        }
                    });
                });
            });
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                if let Some((what, _)) = &self.busy {
                    ui.horizontal(|ui| {
                        ui.add_space(ui.available_width() / 2.0 - 90.0);
                        ui.spinner();
                        ui.label(what);
                    });
                } else if attached_without_room {
                    ui.label(RichText::new("Connected to Prim, waiting for the room…").color(DIM));
                }
                if let Some(e) = &self.error {
                    ui.colored_label(RED, e);
                }
            });
            if !self.rooms.is_empty() && self.busy.is_none() {
                ui.add_space(8.0);
                ui.vertical_centered(|ui| {
                    ui.set_max_width(640.0);
                    ui.label(RichText::new("Rooms you were in").color(DIM));
                    let mut resume = None;
                    for (hex, when) in self.rooms.iter().take(3) {
                        ui.horizontal(|ui| {
                            ui.label(format!("Room {}", &hex[..8]));
                            ui.label(RichText::new(ago(*when)).color(DIM));
                            if ui.small_button("Rejoin").clicked() {
                                resume = Some(hex.clone());
                            }
                        });
                    }
                    if let Some(h) = resume {
                        self.start_room(StartMode::Resume(h), "Rejoining…");
                    }
                });
            }
        });
    }

    fn room_ui(&mut self, root: &mut Ui, s: &Snapshot) {
        let ctx = root.ctx().clone();
        let ctx = &ctx;
        let voice_on = self.voice_running();
        let me_row = s.members.iter().find(|m| m.is_me);
        // ---- toolbar
        egui::Panel::top("toolbar")
            .frame(egui::Frame::new().fill(PANEL).inner_margin(egui::Margin::symmetric(10, 8)))
            .show(root, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let (voice_hint, voice_label) = if voice_on {
                        ("Voice is on. Click to turn it off.", "Voice on")
                    } else {
                        ("Turn voice on: you will hear the room.", "Turn voice on")
                    };
                    if tool_button(ui, Icon::Headphones, voice_label, voice_on.then_some(GREEN), true, voice_hint).clicked() {
                        if voice_on {
                            self.voice.stop();
                        } else {
                            self.voice.start(false);
                        }
                    }
                    let mic_live = voice_on && self.voice.mic;
                    let (mic_icon, mic_label, mic_hint) = if !voice_on {
                        (Icon::MicOff, "Mic", "Turn voice on first")
                    } else if mic_live {
                        (Icon::Mic, "Mute mic", "Your microphone is live: others can hear you. Click to mute.")
                    } else {
                        (Icon::MicOff, "Unmute mic", "You are muted: others cannot hear you. Click to talk.")
                    };
                    if tool_button(
                        ui,
                        mic_icon,
                        mic_label,
                        if voice_on { Some(if mic_live { GREEN } else { RED }) } else { None },
                        voice_on,
                        mic_hint,
                    )
                    .clicked()
                    {
                        let (m, d) = (!self.voice.mic, self.voice.deaf);
                        self.voice.restart_with(m, d);
                    }
                    let deaf = voice_on && self.voice.deaf;
                    if tool_button(
                        ui,
                        if deaf { Icon::HeadphonesOff } else { Icon::Headphones },
                        if deaf { "Undeafen" } else { "Deafen" },
                        deaf.then_some(RED),
                        voice_on,
                        if voice_on { "Silence the room for yourself" } else { "Turn voice on first" },
                    )
                    .clicked()
                    {
                        // Deafening also mutes, as in Mumble and Discord.
                        let d = !self.voice.deaf;
                        let m = self.voice.mic && !d;
                        self.voice.restart_with(m, d);
                    }
                    if tool_button(ui, Icon::Mic, "Audio", self.audio.open.then_some(ACCENT), true, "Pick your microphone and speakers, set gain, and test how you sound").clicked() {
                        if self.audio.open {
                            self.audio.open = false;
                            self.audio.shutdown();
                        } else {
                            self.audio.open();
                        }
                    }
                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(10.0);
                    let can_invite = s.lifecycle == "open";
                    if tool_button(ui, Icon::Invite, "Invite a friend", None, can_invite, "Make a one-time invite link and copy it").clicked() {
                        if let Some(h) = &self.handle {
                            h.send(client::Cmd::Invite);
                        }
                    }
                    if tool_button(ui, Icon::Leave, "Leave", None, true, "Leave this room").clicked() {
                        self.confirm = Some(Confirm::Leave);
                    }
                    if s.am_keyholder && s.lifecycle == "open" {
                        if tool_button(ui, Icon::Door, "Close for everyone", None, true, "End the room for everyone (opener only). Leave just removes you.").clicked() {
                            self.confirm = Some(Confirm::Close);
                        }
                    }
                    let wide = ui.max_rect().width() > 900.0;
                    let adv = |ui: &mut egui::Ui, on: &mut bool| {
                        ui.toggle_value(on, "Advanced view")
                            .on_hover_text("Show the room as an inspectable tree: members, state, channels, leases, routes");
                    };
                    if wide {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| adv(ui, &mut self.advanced));
                    } else {
                        adv(ui, &mut self.advanced);
                    }
                });
                if let Some((_, tok)) = self.invite.clone() {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.colored_label(GREEN, "Invite copied.");
                        ui.label(RichText::new("Send it to one friend (works once, for an hour):").color(DIM));
                        let mut t = tok.clone();
                        ui.add(egui::TextEdit::singleline(&mut t).desired_width(260.0).font(egui::TextStyle::Monospace));
                        if ui.button("Copy again").clicked() {
                            ctx.copy_text(tok);
                        }
                        if ui.small_button("✕").clicked() {
                            self.invite = None;
                        }
                    });
                }
            });
        // ---- footer
        egui::Panel::bottom("footer")
            .frame(egui::Frame::new().fill(PANEL).inner_margin(egui::Margin::symmetric(10, 6)))
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    let (dot, text) = match s.lifecycle.as_str() {
                        "open" => (GREEN, format!("In room {}", &s.room[..6.min(s.room.len())])),
                        "closed" => (AMBER, "This room was closed by its opener".to_string()),
                        _ => (RED, "Not in a room".to_string()),
                    };
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 4.5, dot);
                    ui.label(text);
                    if s.removed {
                        ui.colored_label(RED, "You were removed from this room.");
                    } else if let Some(n) = &s.notice {
                        ui.colored_label(AMBER, n);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Quit Prim").on_hover_text("Leave the room and close everything").clicked() {
                            self.confirm = Some(Confirm::Quit);
                        }
                        ui.label(
                            RichText::new("Closing this window keeps you in the room.").color(DIM).size(12.0),
                        );
                    });
                });
            });
        // ---- left: participants + programs, or the inspector
        let width = ctx.content_rect().width();
        egui::Panel::left("left")
            .resizable(true)
            .default_size(if self.advanced { width * 0.6 } else { width * 0.5 })
            .size_range(300.0..=width * 0.8)
            .frame(egui::Frame::new().fill(BG).inner_margin(10.0))
            .show(root, |ui| {
                if self.advanced {
                    crate::inspector::show(ui, s);
                } else {
                    self.participants(ui, s);
                    ui.add_space(10.0);
                    self.programs(ui, voice_on, me_row.is_some());
                }
            });
        // ---- right: chat
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(10.0))
            .show(root, |ui| self.chat_ui(ui, s));
    }

    fn participants(&mut self, ui: &mut Ui, s: &Snapshot) {
        let n_online = s.members.iter().filter(|m| m.presence != Presence::Offline).count();
        egui::Frame::new()
            .fill(PANEL)
            .stroke(Stroke::new(1.0, LINE))
            .corner_radius(8.0)
            .inner_margin(8.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::hover());
                    icons::paint(ui.painter(), r, Icon::Door, ACCENT);
                    ui.label(RichText::new(format!("Room {}", &s.room[..6.min(s.room.len())])).strong().size(15.0));
                    ui.label(RichText::new(format!("{n_online} here")).color(DIM));
                });
                ui.add_space(2.0);
                for m in &s.members {
                    self.member_row(ui, m);
                }
            });
    }

    fn member_row(&mut self, ui: &mut Ui, m: &MemberRow) {
        let offline = m.presence == Presence::Offline;
        let tint = |c: Color32| if offline { c.gamma_multiply(0.35) } else { c };
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            let dot = match m.presence {
                Presence::Online => GREEN,
                Presence::NotResponding => AMBER,
                Presence::Offline => Color32::from_rgb(90, 96, 108),
            };
            let (r, resp) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
            ui.painter().circle_filled(r.center(), 5.0, dot);
            resp.on_hover_text(match m.presence {
                Presence::Online => "Online",
                Presence::NotResponding => "Not responding",
                Presence::Offline => "Offline",
            });
            // Name (editable for yourself).
            if m.is_me && self.editing_name.is_some() {
                let mut done = None;
                if let Some(buf) = &mut self.editing_name {
                    let r = ui.add(egui::TextEdit::singleline(buf).desired_width(130.0).char_limit(chat::MAX_NAME));
                    if std::mem::take(&mut self.editing_name_focus) {
                        r.request_focus();
                    } else if r.lost_focus() || !r.has_focus() {
                        // Enter or clicking elsewhere keeps the new name; Escape drops it.
                        done = Some(!ui.input(|i| i.key_pressed(egui::Key::Escape)));
                    }
                }
                if let Some(commit) = done {
                    let buf = self.editing_name.take().unwrap_or_default();
                    if commit && !buf.trim().is_empty() {
                        if let Some(h) = &self.handle {
                            h.set_name(&buf);
                        }
                    }
                }
            } else {
                let text = RichText::new(&m.name).size(15.0).color(tint(if m.is_me { TEXT } else { name_color(&m.key) }));
                let text = if m.is_me { text.strong() } else { text };
                ui.label(text).on_hover_text(format!("member {}", m.key));
                if m.is_me {
                    pill(ui, "you", ACCENT);
                    let resp = ui.add(egui::Label::new(RichText::new("rename").size(11.0).color(DIM).underline()).sense(Sense::click()));
                    if resp.on_hover_text("Change your name").clicked() {
                        self.editing_name = Some(m.name.clone());
                        self.editing_name_focus = true;
                    }
                }
                if m.keyholder {
                    pill(ui, "opener", AMBER);
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(4.0);
                // Right to left: last drawn is leftmost. Route text first (rightmost).
                let route = match (&m.route, m.is_me, offline) {
                    (_, true, _) => RichText::new("this computer").color(DIM),
                    (_, _, true) => RichText::new("offline").color(DIM),
                    (Some(r), _, _) => {
                        let rtt = r.rtt_ms.map(|v| format!(" · {v} ms")).unwrap_or_default();
                        let col = if r.path == "direct" { GREEN } else { AMBER };
                        RichText::new(format!("{}{rtt}", r.path)).color(col)
                    }
                    _ => RichText::new("connecting…").color(AMBER),
                };
                ui.label(route.size(12.5));
                ui.add_space(8.0);
                let on = |b: bool, c: Color32| if b { c } else { DIM.gamma_multiply(0.45) };
                // 3D room, player, mic, headphones (drawn right to left).
                small_icon(ui, Icon::Play, on(m.clients.player, ACCENT), if m.clients.player { "Watching a video" } else { "Not watching" });
                small_icon(ui, Icon::Cube, on(m.clients.godot, ACCENT), if m.clients.godot { "In the 3D room" } else { "Not in the 3D room" });
                match m.voice {
                    Some((listening, mic)) => {
                        small_icon(
                            ui,
                            if mic { Icon::Mic } else { Icon::MicOff },
                            if mic { GREEN } else { RED },
                            if mic { "Mic live" } else { "Mic muted" },
                        );
                        let deaf = m.clients.deaf;
                        small_icon(
                            ui,
                            if listening && !deaf { Icon::Headphones } else { Icon::HeadphonesOff },
                            if listening && !deaf { GREEN } else { RED },
                            if deaf { "Deafened" } else if listening { "In voice, listening" } else { "Voice off" },
                        );
                    }
                    None => {
                        small_icon(ui, Icon::MicOff, on(false, DIM), "Not in voice");
                        small_icon(ui, Icon::HeadphonesOff, on(false, DIM), "Not in voice");
                    }
                }
            });
        });
    }

    fn programs(&mut self, ui: &mut Ui, voice_on: bool, _have_me: bool) {
        egui::Frame::new()
            .fill(PANEL)
            .stroke(Stroke::new(1.0, LINE))
            .corner_radius(8.0)
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("Programs on this computer").strong());
                ui.add_space(2.0);
                for spec in CLIENTS {
                    ui.horizontal(|ui| {
                        let avail = spec.unavailable.is_none();
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
                        icons::paint(ui.painter(), r, spec.icon, if avail { TEXT } else { DIM });
                        ui.vertical(|ui| {
                            ui.label(RichText::new(spec.label).color(if avail { TEXT } else { DIM }));
                            let sub = match (spec.unavailable, &spec.kind) {
                                (Some(why), _) => RichText::new(why).color(DIM),
                                (None, ClientKind::Voice) => match (&self.voice.sup.state, voice_on) {
                                    (ProcState::Failed(e), _) => RichText::new(e).color(RED),
                                    (ProcState::Busy, _) => RichText::new("Another program already holds voice for this room").color(AMBER),
                                    (ProcState::Restarting(w), _) => RichText::new(w).color(AMBER),
                                    (_, true) => RichText::new(if self.voice.mic { "On, microphone live" } else { "On, microphone muted" }).color(GREEN),
                                    (_, false) => RichText::new(spec.blurb).color(DIM),
                                },
                                (None, ClientKind::Watch) => match &self.watch.state {
                                    ProcState::Failed(e) => RichText::new(e).color(RED),
                                    _ if self.watch.running() => RichText::new("Playing in mpv").color(GREEN),
                                    _ => RichText::new(spec.blurb).color(DIM),
                                },
                                (None, _) => RichText::new(spec.blurb).color(DIM),
                            };
                            ui.label(sub.size(12.0));
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| match (&spec.kind, spec.unavailable) {
                            (_, Some(_)) => {
                                ui.add_enabled(false, egui::Button::new("Unavailable"));
                            }
                            (ClientKind::Voice, None) => {
                                if ui.button("Audio settings").on_hover_text("Microphone, speakers, gain and a test recording").clicked() {
                                    self.audio.open();
                                }
                                if self.voice.sup.state == ProcState::Busy {
                                    if ui.button("Take over").on_hover_text("Stop the other voice program and use this one").clicked() {
                                        self.voice.start(true);
                                    }
                                } else if voice_on {
                                    if ui.button("Turn off").clicked() {
                                        self.voice.stop();
                                    }
                                } else if ui.button("Turn on").clicked() {
                                    self.voice.start(false);
                                }
                            }
                            (ClientKind::Watch, None) => {
                                let running = self.watch.running();
                                if running {
                                    if ui.button("Stop").on_hover_text("Close mpv and stop watching with the room").clicked() {
                                        self.watch.stop();
                                    }
                                }
                                let typed = !self.watch_url.trim().is_empty();
                                let label = if running { "Load this" } else { "Watch together" };
                                let hint = if typed {
                                    "Everyone in the room who is watching sees this"
                                } else if running {
                                    "Paste a link or a file path first"
                                } else {
                                    "Join what the room is already watching"
                                };
                                let go = ui.add_enabled(!running || typed, egui::Button::new(label)).on_hover_text(hint).clicked();
                                let te = ui.add(
                                    egui::TextEdit::singleline(&mut self.watch_url)
                                        .hint_text("Paste a link or file path")
                                        .desired_width(220.0),
                                );
                                let enter = te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                                if go || (enter && typed) {
                                    let u = self.watch_url.clone();
                                    self.start_watch(u);
                                }
                            }
                            (ClientKind::Program(_), None) => {
                                ui.add_enabled(false, egui::Button::new("Start"));
                            }
                        });
                    });
                    ui.add_space(4.0);
                }
            });
    }

    fn chat_ui(&mut self, ui: &mut Ui, s: &Snapshot) {
        ui.label(RichText::new("Chat").strong().size(15.0));
        ui.label(RichText::new("Only people in the room right now see this. Nothing is saved.").color(DIM).size(12.0));
        ui.add_space(4.0);
        let input_h = 34.0;
        let log_h = (ui.available_height() - input_h - 10.0).max(60.0);
        egui::Frame::new()
            .fill(Color32::from_rgb(18, 20, 25))
            .stroke(Stroke::new(1.0, LINE))
            .corner_radius(8.0)
            .inner_margin(8.0)
            .show(ui, |ui| {
                ui.set_height(log_h - 16.0);
                ui.set_width(ui.available_width());
                egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                    if s.chat.is_empty() {
                        ui.add_space(6.0);
                        ui.label(RichText::new("No messages yet.").color(DIM));
                        ui.label(
                            RichText::new("Say \"test\" to check that your friends can see you, or post \"something's not working\".")
                                .color(DIM)
                                .size(12.5),
                        );
                    }
                    for l in &s.chat {
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            ui.label(RichText::new(chat::clock(l.at, false)).color(DIM).size(12.0).monospace());
                            match &l.from {
                                Some(k) => {
                                    let mine = *k == s.me;
                                    ui.label(
                                        RichText::new(format!("{}:", s.name(k)))
                                            .strong()
                                            .color(if mine { TEXT } else { name_color(k) }),
                                    );
                                    ui.label(&l.text);
                                    match &l.delivery {
                                        Some(Delivery::Pending) => {
                                            ui.label(RichText::new("sending…").color(DIM).size(11.0));
                                        }
                                        Some(Delivery::Done { ok, total, failed }) => {
                                            if !failed.is_empty() {
                                                let who: Vec<String> = failed.iter().map(|(k, st)| format!("{} ({})", s.name(k), st)).collect();
                                                ui.label(RichText::new(format!("not delivered to {}", who.join(", "))).color(RED).size(11.0));
                                            } else if *total == 0 {
                                                ui.label(RichText::new("nobody else is here").color(DIM).size(11.0));
                                            } else {
                                                ui.label(RichText::new(format!("delivered to {ok}")).color(DIM).size(11.0));
                                            }
                                        }
                                        Some(Delivery::Error(e)) => {
                                            ui.label(RichText::new(format!("not sent: {e}")).color(RED).size(11.0));
                                        }
                                        None => {}
                                    }
                                }
                                None => {
                                    ui.label(RichText::new(&l.text).italics().color(DIM));
                                }
                            }
                        });
                    }
                });
            });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let w = ui.available_width() - 70.0;
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.chat_input)
                    .desired_width(w)
                    .hint_text("Type a message and press Enter"),
            );
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let click = ui.add_enabled(!self.chat_input.trim().is_empty(), egui::Button::new("Send")).clicked();
            if enter || click {
                if let Some(h) = &self.handle {
                    h.chat(&self.chat_input);
                }
                self.chat_input.clear();
                r.request_focus();
            }
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(c) = &self.confirm else { return };
        let (title, body, ok): (&str, &str, &str) = match c {
            Confirm::Leave => ("Leave this room?", "You will go offline for everyone. You can rejoin later from \"Rooms you were in\".", "Leave room"),
            Confirm::Close => ("Close the room for everyone?", "Everyone is disconnected and existing invites stop working. You can reopen it later.", "Close room"),
            Confirm::Quit => ("Quit Prim?", "This leaves the room and stops Prim on this computer, including voice. To keep your place, just close the window instead.", "Leave and quit"),
        };
        let mut decision = None;
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_max_width(380.0);
                ui.label(body);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new(ok).color(RED)).clicked() {
                        decision = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        decision = Some(false);
                    }
                });
            });
        match (decision, self.confirm.take()) {
            (Some(true), Some(Confirm::Leave)) => self.leave_room(false),
            (Some(true), Some(Confirm::Quit)) => self.leave_room(true),
            (Some(true), Some(Confirm::Close)) => {
                if let Some(h) = &self.handle {
                    h.send(client::Cmd::Close);
                }
            }
            (None, c) => self.confirm = c,
            _ => {}
        }
    }
}
