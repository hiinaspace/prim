//! The "Audio" settings panel: pick the microphone and speakers (overriding the OS default),
//! see the microphone level, set gain, and record a few seconds to hear back how you sound.
//!
//! The window never opens an audio device itself. Everything goes through `prim voice`
//! children: `prim voice devices` lists, `prim voice meter` publishes the level while the
//! panel is open and voice is off, `prim voice test` runs the loopback, and a running
//! `prim voice` publishes the level itself. Choices are saved per computer in
//! `audio.json` (the same file `prim voice` reads), never in room state.

use std::io::BufRead;
use std::os::unix::fs::FileExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::supervise;

pub const GAIN_DB_RANGE: (f32, f32) = (-20.0, 30.0);
pub const TEST_SECONDS: f64 = 5.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceRef {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub input: Option<DeviceRef>,
    pub output: Option<DeviceRef>,
    pub gain_db: f32,
    pub volume: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { input: None, output: None, gain_db: 0.0, volume: 1.0 }
    }
}

fn dev_json(d: &Option<DeviceRef>) -> Value {
    d.as_ref().map(|d| json!({"id": d.id, "name": d.name})).unwrap_or(Value::Null)
}

fn dev_from(v: &Value) -> Option<DeviceRef> {
    Some(DeviceRef { id: v.get("id")?.as_str()?.into(), name: v.get("name")?.as_str()?.into() })
}

/// The file `prim voice` reads (kept in step with `prim_voice::audio::Settings::path`; a
/// test checks that they agree).
pub fn settings_path() -> PathBuf {
    if let Some(p) = std::env::var_os("PRIM_AUDIO_SETTINGS").filter(|v| !v.is_empty()) {
        return PathBuf::from(p);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("prim").join("audio.json")
}

impl Settings {
    pub fn load() -> Settings {
        Settings::load_from(&settings_path())
    }

    pub fn load_from(path: &Path) -> Settings {
        let Some(v) = std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else {
            return Settings::default();
        };
        Settings {
            input: v.get("input").and_then(dev_from),
            output: v.get("output").and_then(dev_from),
            gain_db: v.get("gain_db").and_then(Value::as_f64).map_or(0.0, |g| (g as f32).clamp(GAIN_DB_RANGE.0, GAIN_DB_RANGE.1)),
            volume: v.get("volume").and_then(Value::as_f64).map_or(1.0, |g| (g as f32).clamp(0.0, 2.0)),
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&settings_path())
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let v = json!({"v": 1, "input": dev_json(&self.input), "output": dev_json(&self.output),
                       "gain_db": self.gain_db, "volume": self.volume});
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&v).unwrap())?;
        std::fs::rename(&tmp, path)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Listing {
    pub inputs: Vec<DeviceRef>,
    pub outputs: Vec<DeviceRef>,
    pub default_input: Option<DeviceRef>,
    pub default_output: Option<DeviceRef>,
}

pub fn parse_listing(text: &str) -> Result<Listing, String> {
    let v: Value = serde_json::from_str(text.trim()).map_err(|e| format!("device list: {e}"))?;
    let list = |k: &str| -> Vec<DeviceRef> { v.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(dev_from).collect()).unwrap_or_default() };
    Ok(Listing {
        inputs: list("inputs"),
        outputs: list("outputs"),
        default_input: v.get("default_input").and_then(dev_from),
        default_output: v.get("default_output").and_then(dev_from),
    })
}

/// Is the saved choice among the devices that exist now? (id first, then name.)
pub fn is_connected(want: &DeviceRef, avail: &[DeviceRef]) -> bool {
    avail.iter().any(|d| d.id == want.id) || avail.iter().any(|d| d.name == want.name)
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Reading {
    pub seq: u32,
    pub peak: f32,
    pub rms: f32,
    pub clips: u32,
    pub out_peak: f32,
}

pub fn read_levels(path: &Path) -> Option<Reading> {
    let f = std::fs::File::open(path).ok()?;
    let mut b = [0u8; 32];
    f.read_exact_at(&mut b, 0).ok()?;
    if &b[0..4] != b"LVL1" {
        return None;
    }
    let u = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    Some(Reading { seq: u(4), peak: f32::from_bits(u(8)), rms: f32::from_bits(u(12)), clips: u(16), out_peak: f32::from_bits(u(20)) })
}

/// Helpers die with the window: unlike voice (which keeps you in the room), the meter and
/// the test only make sense while the panel is on screen.
fn die_with_parent(c: &mut Command) -> &mut Command {
    unsafe {
        c.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    c
}

/// Frequency of the private tone "microphone" in test mode (0: silence).
fn test_tone() -> String {
    std::env::var("PRIM_UI_AUDIO_TONE").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "440".into())
}

pub fn to_db(x: f32) -> f32 {
    20.0 * x.max(1e-6).log10()
}

/// 0..1 position on a -60..0 dBFS scale.
pub fn meter_pos(x: f32) -> f32 {
    ((to_db(x) + 60.0) / 60.0).clamp(0.0, 1.0)
}

#[derive(Clone, Debug, PartialEq)]
pub enum TestState {
    Idle,
    Recording { t: f64, of: f64 },
    Playing { since: Instant, of: f64 },
    Done { bytes: u64, silent: bool },
    Failed(String),
}

struct Test {
    child: Child,
    rx: Receiver<Value>,
}

/// The panel's state and the children it runs.
pub struct Panel {
    pub open: bool,
    pub settings: Settings,
    pub listing: Option<Listing>,
    pub listing_error: Option<String>,
    listing_rx: Option<Receiver<Result<Listing, String>>>,
    meter: Option<Child>,
    test: Option<Test>,
    pub test_state: TestState,
    pub reading: Option<Reading>,
    last_seq: (u32, Instant),
    /// The meter child or voice child is publishing (the sequence number moved lately).
    pub live: bool,
    pub peak_hold: f32,
    peak_hold_at: Instant,
    clips_seen: u32,
    pub clip_until: Option<Instant>,
    saved_at: Instant,
    socket: PathBuf,
    test_mode: bool,
}

impl Panel {
    pub fn new(socket: &Path, test_mode: bool) -> Panel {
        Panel {
            open: false,
            settings: Settings::load(),
            listing: None,
            listing_error: None,
            listing_rx: None,
            meter: None,
            test: None,
            test_state: TestState::Idle,
            reading: None,
            last_seq: (0, Instant::now()),
            live: false,
            peak_hold: 0.0,
            peak_hold_at: Instant::now(),
            clips_seen: 0,
            clip_until: None,
            saved_at: Instant::now(),
            socket: socket.to_path_buf(),
            test_mode,
        }
    }

    pub fn open(&mut self) {
        self.open = true;
        self.settings = Settings::load();
        self.refresh_devices();
    }

    pub fn refresh_devices(&mut self) {
        if self.listing_rx.is_some() {
            return;
        }
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let r = if let Some(j) = std::env::var("PRIM_UI_AUDIO_DEVICES").ok().filter(|s| !s.is_empty()) {
                parse_listing(&j)
            } else {
                Command::new(supervise::exe())
                    .args(["voice", "devices"])
                    .stdin(Stdio::null())
                    .stderr(Stdio::null())
                    .output()
                    .map_err(|e| e.to_string())
                    .and_then(|o| if o.status.success() { parse_listing(&String::from_utf8_lossy(&o.stdout)) } else { Err("could not list audio devices".into()) })
            };
            let _ = tx.send(r);
        });
        self.listing_rx = Some(rx);
    }

    /// Save after a change (rate-limited while a slider drags; `force` on release).
    pub fn save(&mut self, force: bool) {
        if force || self.saved_at.elapsed() > Duration::from_millis(150) {
            self.saved_at = Instant::now();
            let _ = self.settings.save();
        }
    }

    pub fn start_test(&mut self) {
        self.stop_test();
        self.stop_meter(); // the test reads the mic itself and publishes the same levels
        let mut args: Vec<String> = vec!["voice".into(), "test".into(), format!("--seconds={TEST_SECONDS}"), format!("--levels={}", supervise::meter_levels_path(&self.socket).display())];
        if self.test_mode {
            args.push(format!("--capture=tone:{}", test_tone()));
            args.push("--output=null".into());
        }
        let spawned = die_with_parent(Command::new(supervise::exe()).args(&args))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn();
        match spawned {
            Ok(mut child) => {
                let out = child.stdout.take().unwrap();
                let (tx, rx) = channel();
                std::thread::spawn(move || {
                    for l in std::io::BufReader::new(out).lines().map_while(|r| r.ok()) {
                        if let Ok(v) = serde_json::from_str::<Value>(l.as_str()) {
                            let _ = tx.send(v);
                        }
                    }
                });
                self.test = Some(Test { child, rx });
                self.test_state = TestState::Recording { t: 0.0, of: TEST_SECONDS };
            }
            Err(e) => self.test_state = TestState::Failed(e.to_string()),
        }
    }

    pub fn stop_test(&mut self) {
        if let Some(mut t) = self.test.take() {
            supervise::terminate(t.child.id() as i32);
            std::thread::spawn(move || {
                let _ = t.child.wait();
            });
        }
        if matches!(self.test_state, TestState::Recording { .. } | TestState::Playing { .. }) {
            self.test_state = TestState::Idle;
        }
    }

    fn stop_meter(&mut self) {
        if let Some(mut c) = self.meter.take() {
            supervise::terminate(c.id() as i32);
            std::thread::spawn(move || {
                let _ = c.wait();
            });
        }
    }

    fn start_meter(&mut self) {
        let mut args: Vec<String> = vec!["voice".into(), "meter".into(), format!("--levels={}", supervise::meter_levels_path(&self.socket).display())];
        if self.test_mode {
            args.push(format!("--capture=tone:{}", test_tone()));
        }
        self.meter = die_with_parent(Command::new(supervise::exe()).args(&args))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .ok();
    }

    /// The meter child must follow the settings that choose the device.
    pub fn restart_meter(&mut self) {
        self.stop_meter();
    }

    /// Once a frame. `voice_mic_live`: a running `prim voice` has the microphone open and
    /// publishes the level itself.
    pub fn tick(&mut self, voice_mic_live: bool) {
        if let Some(rx) = &self.listing_rx {
            if let Ok(r) = rx.try_recv() {
                self.listing_rx = None;
                match r {
                    Ok(l) => {
                        self.listing = Some(l);
                        self.listing_error = None;
                    }
                    Err(e) => self.listing_error = Some(e),
                }
            }
        }
        if let Some(t) = &mut self.test {
            while let Ok(v) = t.rx.try_recv() {
                self.test_state = match v.get("phase").and_then(Value::as_str) {
                    Some("recording") => TestState::Recording { t: v["t"].as_f64().unwrap_or(0.0), of: v["of"].as_f64().unwrap_or(TEST_SECONDS) },
                    Some("playing") => TestState::Playing { since: Instant::now(), of: v["of"].as_f64().unwrap_or(TEST_SECONDS) },
                    Some("done") => TestState::Done { bytes: v["bytes"].as_u64().unwrap_or(0), silent: v["silent"].as_bool().unwrap_or(false) },
                    _ => continue,
                };
            }
            if let Ok(Some(st)) = t.child.try_wait() {
                self.test = None;
                if matches!(self.test_state, TestState::Recording { .. } | TestState::Playing { .. }) {
                    self.test_state = TestState::Failed(if st.success() { "stopped early".into() } else { "could not open the microphone or speakers".into() });
                }
            }
        }
        let testing = self.test.is_some();
        let want_meter = self.open && !voice_mic_live && !testing;
        if let Some(c) = &mut self.meter {
            if matches!(c.try_wait(), Ok(Some(_))) {
                self.meter = None;
            }
        }
        if want_meter && self.meter.is_none() {
            self.start_meter();
        } else if !want_meter && self.meter.is_some() {
            self.stop_meter();
        }
        // Read the level from whoever is publishing it.
        let path = if voice_mic_live { supervise::voice_levels_path(&self.socket) } else { supervise::meter_levels_path(&self.socket) };
        if self.open {
            let r = read_levels(&path);
            if let Some(r) = r {
                if r.seq != self.last_seq.0 {
                    self.last_seq = (r.seq, Instant::now());
                }
                if r.clips > self.clips_seen && self.clips_seen != 0 || (r.clips > 0 && self.clips_seen == 0 && self.reading.is_some()) {
                    self.clip_until = Some(Instant::now() + Duration::from_millis(1500));
                }
                self.clips_seen = r.clips;
                if r.peak >= self.peak_hold || self.peak_hold_at.elapsed() > Duration::from_millis(1200) {
                    self.peak_hold = r.peak;
                    self.peak_hold_at = Instant::now();
                }
            }
            self.live = r.is_some() && self.last_seq.1.elapsed() < Duration::from_millis(500);
            self.reading = if self.live { r } else { None };
            if matches!(self.test_state, TestState::Playing { .. }) {
                // The microphone is closed during playback: show what is going to the speakers.
                if let Some(r) = &mut self.reading {
                    r.peak = r.out_peak;
                    r.rms = r.out_peak;
                    self.peak_hold = self.peak_hold.max(r.out_peak);
                }
            }
        }
    }

    /// Window closed or voice turned off for good: stop helper children.
    pub fn shutdown(&mut self) {
        self.stop_meter();
        self.stop_test();
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(feature = "gui")]
pub use gui::show;

#[cfg(feature = "gui")]
mod gui {
    use super::*;
    use eframe::egui::{self, Align2, Color32, RichText, Sense, Stroke, StrokeKind, Vec2};

    const DIM: Color32 = Color32::from_rgb(138, 146, 160);
    const GREEN: Color32 = Color32::from_rgb(88, 200, 130);
    const AMBER: Color32 = Color32::from_rgb(232, 178, 70);
    const RED: Color32 = Color32::from_rgb(235, 98, 88);
    const TRACK: Color32 = Color32::from_rgb(18, 20, 25);
    const LINE: Color32 = Color32::from_rgb(52, 57, 68);

    fn meter(ui: &mut egui::Ui, p: &Panel) {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width() - 70.0, 16.0), Sense::hover());
        let pt = ui.painter();
        pt.rect(rect, 3.0, TRACK, Stroke::new(1.0, LINE), StrokeKind::Inside);
        let x = |v: f32| rect.left() + rect.width() * meter_pos(v);
        if let Some(r) = p.reading {
            // Bar: RMS-ish level, coloured by zone; tick: peak hold.
            let top = x(r.rms);
            for (lo, hi, c) in [(-60.0, -12.0, GREEN), (-12.0, -3.0, AMBER), (-3.0, 0.0, RED)] {
                let (a, b) = (rect.left() + rect.width() * ((lo + 60.0) / 60.0), rect.left() + rect.width() * ((hi + 60.0) / 60.0));
                if top > a {
                    pt.rect_filled(egui::Rect::from_min_max(egui::pos2(a, rect.top() + 2.0), egui::pos2(top.min(b), rect.bottom() - 2.0)), 1.0, c);
                }
            }
            let hx = x(p.peak_hold);
            pt.line_segment([egui::pos2(hx, rect.top() + 1.0), egui::pos2(hx, rect.bottom() - 1.0)], Stroke::new(2.0, Color32::WHITE));
        }
        for db in [-48.0f32, -36.0, -24.0, -12.0, -6.0] {
            let tx = rect.left() + rect.width() * ((db + 60.0) / 60.0);
            pt.line_segment([egui::pos2(tx, rect.bottom() - 4.0), egui::pos2(tx, rect.bottom() - 1.0)], Stroke::new(1.0, DIM));
        }
        let clipping = p.clip_until.is_some_and(|t| t > Instant::now());
        let clip = egui::Rect::from_min_size(egui::pos2(rect.right() + 6.0, rect.top()), Vec2::new(52.0, 16.0));
        pt.rect(clip, 3.0, if clipping { RED } else { TRACK }, Stroke::new(1.0, if clipping { RED } else { LINE }), StrokeKind::Inside);
        pt.text(clip.center(), Align2::CENTER_CENTER, "CLIP", egui::FontId::proportional(11.0), if clipping { Color32::WHITE } else { DIM });
        ui.advance_cursor_after_rect(clip);
    }

    fn device_combo(ui: &mut egui::Ui, id: &str, input: bool, p: &mut Panel) -> bool {
        let (list, default, saved) = match &p.listing {
            Some(l) if input => (l.inputs.clone(), l.default_input.clone(), p.settings.input.clone()),
            Some(l) => (l.outputs.clone(), l.default_output.clone(), p.settings.output.clone()),
            None => (vec![], None, if input { p.settings.input.clone() } else { p.settings.output.clone() }),
        };
        let missing = p.listing.is_some() && saved.as_ref().is_some_and(|s| !is_connected(s, &list));
        let default_label = match &default {
            Some(d) => format!("System default ({})", d.name),
            None => "System default".to_string(),
        };
        let shown = match &saved {
            None => default_label.clone(),
            Some(s) if missing => format!("{} (not connected)", s.name),
            Some(s) => s.name.clone(),
        };
        let mut chosen: Option<Option<DeviceRef>> = None;
        egui::ComboBox::from_id_salt(id)
            .width(ui.available_width() - 8.0)
            .selected_text(shown)
            .show_ui(ui, |ui| {
                if ui.selectable_label(saved.is_none(), &default_label).clicked() {
                    chosen = Some(None);
                }
                for d in &list {
                    if ui.selectable_label(saved.as_ref() == Some(d), &d.name).clicked() {
                        chosen = Some(Some(d.clone()));
                    }
                }
            });
        if missing {
            let s = saved.as_ref().unwrap();
            ui.label(RichText::new(format!("\"{}\" is not connected. Using the system default for now; Prim switches back when it returns.", s.name)).color(AMBER).size(12.0));
        }
        if let Some(c) = chosen {
            if input {
                p.settings.input = c;
            } else {
                p.settings.output = c;
            }
            p.save(true);
            return true;
        }
        false
    }

    /// Draws the panel. Returns true when a device choice changed (the caller restarts
    /// voice so it opens the new device).
    pub fn show(ctx: &egui::Context, p: &mut Panel) -> bool {
        let mut devices_changed = false;
        let mut open = p.open;
        egui::Window::new("Audio")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_TOP, [0.0, 50.0])
            .fixed_size([440.0, 0.0])
            .show(ctx, |ui| {
                ui.spacing_mut().slider_width = 200.0;
                ui.label(RichText::new("These choices are used instead of your computer's sound settings, and are remembered on this computer.").color(DIM).size(12.0));
                ui.add_space(8.0);
                ui.label(RichText::new("Microphone").strong());
                if device_combo(ui, "audio-mic", true, p) {
                    devices_changed = true;
                    p.restart_meter();
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    meter(ui, p);
                });
                ui.label(
                    RichText::new(match p.reading {
                        Some(r) if matches!(p.test_state, TestState::Playing { .. }) => format!("Playing back: output level {:.0} dB.", to_db(r.out_peak)),
                        Some(r) => format!("Level {:.0} dB, peak {:.0} dB. Speak normally: the bar should reach the amber zone and stay out of red. (0 dB is the loudest possible.)", to_db(r.rms), to_db(p.peak_hold)),
                        None if p.test_mode_hint() => "Starting the meter…".to_string(),
                        None => "Starting the meter… (it listens to the microphone while this window is open)".to_string(),
                    })
                    .color(DIM)
                    .size(12.0),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label("Input gain");
                    let r = ui.add(egui::Slider::new(&mut p.settings.gain_db, GAIN_DB_RANGE.0..=GAIN_DB_RANGE.1).step_by(0.5).suffix(" dB").fixed_decimals(1));
                    if r.changed() {
                        p.save(r.drag_stopped());
                    }
                    if r.drag_stopped() {
                        p.save(true);
                    }
                    if ui.small_button("Reset").clicked() {
                        p.settings.gain_db = 0.0;
                        p.save(true);
                    }
                });
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);
                ui.label(RichText::new("Speakers").strong());
                if device_combo(ui, "audio-spk", false, p) {
                    devices_changed = true;
                }
                ui.horizontal(|ui| {
                    ui.label("Volume");
                    let mut pct = p.settings.volume * 100.0;
                    let r = ui.add(egui::Slider::new(&mut pct, 0.0..=200.0).step_by(1.0).suffix(" %").fixed_decimals(0));
                    if r.changed() {
                        p.settings.volume = pct / 100.0;
                        p.save(r.drag_stopped());
                    }
                    if r.drag_stopped() {
                        p.save(true);
                    }
                    if ui.small_button("Reset").clicked() {
                        p.settings.volume = 1.0;
                        p.save(true);
                    }
                });
                if let Some(e) = &p.listing_error {
                    ui.label(RichText::new(e).color(RED).size(12.0));
                }
                ui.horizontal(|ui| {
                    if ui.small_button("Refresh devices").clicked() {
                        p.refresh_devices();
                    }
                });
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(6.0);
                ui.label(RichText::new("Test your microphone").strong());
                ui.label(RichText::new(format!("Records {TEST_SECONDS:.0} seconds, then plays it back through your speakers the way a friend would hear it: after your gain, compressed for the network, and decoded again. Wear headphones so it does not feed back.")).color(DIM).size(12.0));
                ui.add_space(4.0);
                let state = p.test_state.clone();
                ui.horizontal(|ui| {
                    match &state {
                        TestState::Recording { .. } | TestState::Playing { .. } => {
                            if ui.button("Cancel").clicked() {
                                p.stop_test();
                            }
                        }
                        _ => {
                            if ui.button(RichText::new("Test microphone").strong()).clicked() {
                                p.start_test();
                            }
                        }
                    }
                    match &state {
                        TestState::Idle => {}
                        TestState::Recording { t, of } => {
                            ui.label(RichText::new(format!("Recording… speak now  {:.1} s left", (of - t).max(0.0))).color(RED));
                        }
                        TestState::Playing { since, of } => {
                            ui.label(RichText::new(format!("Playing back…  {:.1} s left", (of - since.elapsed().as_secs_f64()).max(0.0))).color(GREEN));
                        }
                        TestState::Done { silent: true, .. } => {
                            ui.label(RichText::new("Heard nothing. Check the microphone choice and raise the gain.").color(AMBER));
                        }
                        TestState::Done { bytes, .. } => {
                            ui.label(RichText::new(format!("Done. {:.1} kB would have been sent.", *bytes as f64 / 1000.0)).color(DIM));
                        }
                        TestState::Failed(e) => {
                            ui.label(RichText::new(e).color(RED));
                        }
                    }
                });
                if matches!(state, TestState::Recording { .. } | TestState::Playing { .. }) {
                    let frac = match &state {
                        TestState::Recording { t, of } => (*t / *of / 2.0) as f32,
                        TestState::Playing { since, of } => (0.5 + since.elapsed().as_secs_f64() / *of / 2.0).min(1.0) as f32,
                        _ => 0.0,
                    };
                    ui.add_space(4.0);
                    ui.add(egui::ProgressBar::new(frac).desired_height(8.0));
                }
                if matches!(state, TestState::Done { .. }) {
                    ui.label(RichText::new("It does not include your position in the room, network delay or packet loss.").color(DIM).size(12.0));
                }
            });
        if !open && p.open {
            p.shutdown();
        }
        p.open = open;
        ctx.request_repaint_after(Duration::from_millis(33));
        devices_changed
    }

    impl Panel {
        fn test_mode_hint(&self) -> bool {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window and `prim voice` share one settings file: each reads what the other writes.
    #[test]
    fn settings_file_is_shared_with_prim_voice() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("audio.json");
        let v = prim_voice::audio::Settings {
            input: Some(prim_voice::audio::DeviceRef { id: "i".into(), name: "In".into() }),
            output: None,
            gain_db: 4.5,
            volume: 0.8,
        };
        v.save_to(&p).unwrap();
        let w = Settings::load_from(&p);
        assert_eq!((w.input.as_ref().map(|d| d.name.as_str()), w.output.is_none(), w.gain_db, w.volume), (Some("In"), true, 4.5, 0.8));
        Settings { input: None, output: Some(DeviceRef { id: "o".into(), name: "Out".into() }), gain_db: -3.0, volume: 1.5 }.save_to(&p).unwrap();
        let v2 = prim_voice::audio::Settings::load_from(&p);
        assert_eq!((v2.input.is_none(), v2.output.map(|d| d.name), v2.gain_db, v2.volume), (true, Some("Out".to_string()), -3.0, 1.5));
    }

    #[test]
    fn missing_device_is_detected_by_id_or_name() {
        let avail = vec![DeviceRef { id: "a".into(), name: "A".into() }];
        assert!(is_connected(&DeviceRef { id: "a".into(), name: "x".into() }, &avail));
        assert!(is_connected(&DeviceRef { id: "zz".into(), name: "A".into() }, &avail));
        assert!(!is_connected(&DeviceRef { id: "zz".into(), name: "Gone".into() }, &avail));
    }
}
