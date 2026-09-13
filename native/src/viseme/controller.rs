use super::{
    frontend::Frontend,
    model::{Model, State},
};
use godot::{
    classes::{INode, Node, Os, ProjectSettings},
    prelude::*,
};
use godot_network_audio::{AudioStreamNetwork, NetworkAudioSender, PcmTap};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

struct Frame {
    weights: [f32; 15],
    at: Instant,
    generation: u64,
    sample_end: u64,
}
struct Source {
    tap: Arc<PcmTap>,
    frame: Mutex<Option<Frame>>,
    hops: AtomicU64,
    max_us: AtomicU64,
}
impl Source {
    fn new(tap: Arc<PcmTap>) -> Arc<Self> {
        Arc::new(Self {
            tap,
            frame: Mutex::new(None),
            hops: AtomicU64::new(0),
            max_us: AtomicU64::new(0),
        })
    }
}
enum Command {
    Add(String, Arc<Source>),
    Remove(String),
}
struct Context {
    source: Arc<Source>,
    frontend: Option<Frontend>,
    state: Option<State>,
    rate: u32,
    generation: u64,
    next: u64,
    last: Instant,
    asleep: bool,
}
impl Context {
    fn new(source: Arc<Source>) -> Self {
        Self {
            source,
            frontend: None,
            state: None,
            rate: 0,
            generation: 0,
            next: 0,
            last: Instant::now(),
            asleep: true,
        }
    }
    fn clear(&mut self) {
        self.state = None;
        if let Some(f) = self.frontend.as_mut() {
            f.reset();
        }
        *self.source.frame.lock().unwrap() = None;
        self.asleep = true;
    }
    fn process(&mut self, model: &mut Model) -> anyhow::Result<()> {
        // Bounded work per source. Old blocks are discarded, never replayed after stalls.
        for _ in 0..128 {
            let Some(block) = self.source.tap.pop() else {
                break;
            };
            if block.generation != self.source.tap.generation()
                || block.captured_at.elapsed() > Duration::from_millis(100)
            {
                self.clear();
                continue;
            }
            if !(8000..=192000).contains(&block.sample_rate) {
                self.clear();
                continue;
            }
            let discontinuity = self.asleep
                || self.generation != block.generation
                || self.next != block.sample_start
                || self.rate != block.sample_rate;
            if self.rate != block.sample_rate {
                self.frontend = Some(Frontend::new(block.sample_rate));
                self.rate = block.sample_rate;
            }
            if discontinuity {
                self.frontend.as_mut().unwrap().reset();
                self.state = Some(model.state()?);
            }
            self.asleep = false;
            self.generation = block.generation;
            self.next = block.sample_start + block.len as u64;
            self.last = block.captured_at;
            let start = Instant::now();
            let frontend = self.frontend.as_mut().unwrap();
            frontend.push(&block.samples[..block.len]);
            while let Some(mel) = frontend.next_mel() {
                let weights = model.step(self.state.as_mut().unwrap(), mel)?;
                self.source.hops.fetch_add(1, Ordering::Relaxed);
                *self.source.frame.lock().unwrap() = Some(Frame {
                    weights,
                    at: block.captured_at,
                    generation: block.generation,
                    sample_end: self.next,
                });
            }
            self.source
                .max_us
                .fetch_max(start.elapsed().as_micros() as u64, Ordering::Relaxed);
        }
        if !self.asleep
            && (self.last.elapsed() > Duration::from_millis(150)
                || self.generation != self.source.tap.generation())
        {
            self.clear();
        }
        Ok(())
    }
}
#[derive(GodotClass)]
#[class(base=Node)]
pub struct PrimVisemes {
    base: Base<Node>,
    sources: HashMap<String, Arc<Source>>,
    tx: Option<mpsc::Sender<Command>>,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<String>>,
    worker: Option<JoinHandle<()>>,
    warned: bool,
}
#[godot_api]
impl INode for PrimVisemes {
    fn init(base: Base<Node>) -> Self {
        Self {
            base,
            sources: HashMap::new(),
            tx: None,
            stop: Arc::new(AtomicBool::new(false)),
            status: Arc::new(Mutex::new("loading".into())),
            worker: None,
            warned: false,
        }
    }
    fn ready(&mut self) {
        let path = runtime_path();
        let (tx, rx) = mpsc::channel();
        self.tx = Some(tx);
        let stop = self.stop.clone();
        let status = self.status.clone();
        match thread::Builder::new()
            .name("prim-visemes".into())
            .spawn(move || {
                let result = (|| -> anyhow::Result<()> {
                    let mut model = Model::load(&path)?;
                    *status.lock().unwrap() = "ready".into();
                    let mut contexts = HashMap::<String, Context>::new();
                    while !stop.load(Ordering::Acquire) {
                        let apply =
                            |cmd: Command, contexts: &mut HashMap<String, Context>| match cmd {
                                Command::Add(id, s) => {
                                    contexts.insert(id, Context::new(s));
                                }
                                Command::Remove(id) => {
                                    contexts.remove(&id);
                                }
                            };
                        match rx.recv_timeout(Duration::from_millis(2)) {
                            Ok(c) => apply(c, &mut contexts),
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            Err(_) => {}
                        }
                        for c in rx.try_iter() {
                            apply(c, &mut contexts);
                        }
                        for context in contexts.values_mut() {
                            context.process(&mut model)?;
                        }
                    }
                    Ok(())
                })();
                if let Err(e) = result {
                    *status.lock().unwrap() = format!("unavailable: {e:#}");
                }
            }) {
            Ok(w) => self.worker = Some(w),
            Err(e) => *self.status.lock().unwrap() = format!("unavailable: {e}"),
        }
    }
    fn process(&mut self, _: f64) {
        let status = self.status.lock().unwrap().clone();
        if !self.warned && status.starts_with("unavailable") {
            self.warned = true;
            godot_warn!("Lip sync {status}; voice playback remains enabled");
        }
    }
    fn exit_tree(&mut self) {
        self.shutdown();
    }
}
impl PrimVisemes {
    fn attach(&mut self, id: String, tap: Arc<PcmTap>) {
        if id.len() > 128 || (!self.sources.contains_key(&id) && self.sources.len() >= 6) {
            return;
        }
        tap.enable();
        let source = Source::new(tap);
        if let Some(tx) = &self.tx {
            let _ = tx.send(Command::Add(id.clone(), source.clone()));
        }
        self.sources.insert(id, source);
    }
    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.tx = None;
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
        self.sources.clear();
    }
}
impl Drop for PrimVisemes {
    fn drop(&mut self) {
        self.shutdown();
    }
}
#[godot_api]
impl PrimVisemes {
    #[func]
    fn attach_local(&mut self, sender: Gd<NetworkAudioSender>) {
        self.attach("local".into(), sender.bind().pcm_tap());
    }
    #[func]
    fn attach_remote(&mut self, id: GString, stream: Gd<AudioStreamNetwork>) {
        if id != "local" {
            self.attach(id.to_string(), stream.bind().pcm_tap());
        }
    }
    #[func]
    fn remove_source(&mut self, id: GString) {
        let id = id.to_string();
        self.sources.remove(&id);
        if let Some(tx) = &self.tx {
            let _ = tx.send(Command::Remove(id));
        }
    }
    #[func]
    fn reset_source(&mut self, id: GString) {
        if let Some(s) = self.sources.get(&id.to_string()) {
            s.tap.reset();
        }
    }
    #[func]
    fn get_weights(&self, id: GString) -> PackedFloat32Array {
        let zero = [0.0; 15];
        let weights = self
            .sources
            .get(&id.to_string())
            .and_then(|s| {
                let guard = s.frame.try_lock().ok()?;
                let f = guard.as_ref()?;
                (f.generation == s.tap.generation() && f.at.elapsed() < Duration::from_millis(150))
                    .then_some(f.weights)
            })
            .unwrap_or(zero);
        PackedFloat32Array::from(weights.as_slice())
    }
    #[func]
    fn get_status(&self) -> GString {
        self.status.lock().unwrap().as_str().into()
    }
    #[func]
    fn get_stats(&self, id: GString) -> VarDictionary {
        let mut d = VarDictionary::new();
        d.set("status", &self.get_status());
        if let Some(s) = self.sources.get(&id.to_string()) {
            d.set("hops", s.hops.load(Ordering::Relaxed) as i64);
            d.set("max_block_us", s.max_us.load(Ordering::Relaxed) as i64);
            d.set("dropped_samples", s.tap.dropped_samples() as i64);
            if let Ok(guard) = s.frame.try_lock() {
                if let Some(f) = guard.as_ref() {
                    d.set("age_ms", f.at.elapsed().as_millis() as i64);
                    d.set("sample_end", f.sample_end as i64);
                }
            }
        }
        d
    }
    /// Offline fixtures use the same bounded tap and worker, without microphone access.
    #[func]
    fn push_test_pcm(&mut self, id: GString, samples: PackedFloat32Array, rate: i32) {
        if !(8000..=192000).contains(&rate) {
            return;
        }
        let key = id.to_string();
        if !self.sources.contains_key(&key) {
            self.attach(key.clone(), Arc::new(PcmTap::default()));
        }
        if let Some(s) = self.sources.get(&key) {
            s.tap.push(samples.as_slice(), rate as u32);
        }
    }
}
fn runtime_path() -> PathBuf {
    if let Some(p) = std::env::var_os("PRIM_ONNXRUNTIME_LIBRARY") {
        return p.into();
    }
    let (platform, name) = if cfg!(target_os = "windows") {
        ("windows", "onnxruntime.dll")
    } else {
        ("linux", "libonnxruntime.so")
    };
    let resource = PathBuf::from(
        ProjectSettings::singleton()
            .globalize_path(&format!("res://bin/{platform}/{name}"))
            .to_string(),
    );
    if resource.is_file() {
        return resource;
    }
    let executable = PathBuf::from(Os::singleton().get_executable_path().to_string());
    let root = executable.parent().unwrap_or(std::path::Path::new("."));
    for path in [root.join(name), root.join("lib").join(name)] {
        if path.is_file() {
            return path;
        }
    }
    resource
}
