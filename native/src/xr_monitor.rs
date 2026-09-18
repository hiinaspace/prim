//! Optional, read-only Monado observation. IPC never runs on Godot's frame thread.
use godot::prelude::*;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

#[derive(GodotClass)]
#[class(base=RefCounted)]
pub struct PrimXRMonitor {
    snapshot: Arc<Mutex<(Instant, Value)>>,
    running: Arc<AtomicBool>,
    connected_runtime: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    base: Base<RefCounted>,
}

#[godot_api]
impl IRefCounted for PrimXRMonitor {
    fn init(base: Base<RefCounted>) -> Self {
        let snapshot = Arc::new(Mutex::new((
            Instant::now(),
            json!({"activity":"unknown", "runtime":"Undetected"}),
        )));
        let running = Arc::new(AtomicBool::new(true));
        let connected_runtime = Arc::new(AtomicBool::new(false));
        let (out, run, active) = (snapshot.clone(), running.clone(), connected_runtime.clone());
        let worker = std::thread::Builder::new().name("prim-xr-status".into()).spawn(move || {
            #[cfg(target_os = "linux")]
            let mut monado = None;
            while run.load(Ordering::Relaxed) {
                let mut state = configured_runtime();
                #[cfg(target_os = "linux")]
                {
                    let selected = state["runtime"] == "Monado" || active.load(Ordering::Relaxed);
                    // A socket may be socket-activated. Do not connect merely because it exists.
                    let service_up = std::fs::read_dir("/proc").is_ok_and(|entries| entries.flatten().any(|entry| {
                        std::fs::read_to_string(entry.path().join("comm")).is_ok_and(|s| s.trim() == "monado-service")
                    }));
                    if selected && (service_up || active.load(Ordering::Relaxed)) {
                        if monado.is_none() { monado = unsafe { Monado::open().ok() }; }
                        state = match monado.as_mut().and_then(|m| unsafe { m.poll().ok() }) {
                            Some(clients) => classify_clients(&clients),
                            None => { monado = None; json!({"runtime":"Monado", "activity":"unknown", "detail":"Client status unavailable"}) }
                        };
                    } else {
                        monado = None;
                        if selected { state["activity"] = json!("unavailable"); }
                    }
                }
                #[cfg(not(target_os = "linux"))]
                let _ = &active;
                if let Ok(mut dst) = out.lock() { *dst = (Instant::now(), state); }
                // Short sleeps keep normal shutdown responsive. Never unload the extension
                // with one of its worker threads still executing code.
                for _ in 0..10 {
                    if !run.load(Ordering::Relaxed) { break; }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }).ok();
        Self {
            snapshot,
            running,
            connected_runtime,
            worker,
            base,
        }
    }
}
impl Drop for PrimXRMonitor {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
#[godot_api]
impl PrimXRMonitor {
    #[func]
    fn set_monado_session_active(&mut self, active: bool) {
        self.connected_runtime.store(active, Ordering::Relaxed);
    }
    #[func]
    fn snapshot_json(&self) -> GString {
        let Ok(snapshot) = self.snapshot.lock() else {
            return "{\"activity\":\"unknown\"}".into();
        };
        let mut value = snapshot.1.clone();
        value["age_ms"] = json!(snapshot.0.elapsed().as_millis() as u64);
        GString::from(value.to_string().as_str())
    }
}

fn configured_runtime() -> Value {
    let mut paths = Vec::new();
    if let Some(path) = std::env::var_os("XR_RUNTIME_JSON") {
        paths.push(std::path::PathBuf::from(path));
    } else {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
            });
        if let Some(config) = config {
            paths.push(config.join("openxr/1/active_runtime.json"));
        }
        for path in std::env::var("XDG_CONFIG_DIRS")
            .unwrap_or_else(|_| "/etc/xdg".into())
            .split(':')
        {
            paths.push(std::path::Path::new(path).join("openxr/1/active_runtime.json"));
        }
    }
    for path in paths {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                let library = value["runtime"]["library_path"]
                    .as_str()
                    .unwrap_or("")
                    .to_lowercase();
                let name = if library.contains("monado") {
                    "Monado"
                } else if library.contains("steam") || library.contains("vrclient") {
                    "SteamVR"
                } else {
                    "OpenXR"
                };
                return json!({"runtime":name, "activity":"unknown"});
            }
        }
    }
    json!({"runtime":"Undetected", "activity":"unknown"})
}

fn classify_clients(clients: &[(u32, String, u32)]) -> Value {
    // Ignore *all* overlays, not only known app names. A live unfocused main app
    // still owns gameplay input; visibility alone is not evidence it has exited.
    let games: Vec<_> = clients
        .iter()
        .filter(|(_, _, flags)| flags & 2 != 0 && flags & 16 == 0)
        .collect();
    let current = games
        .iter()
        .find(|(_, _, flags)| flags & 1 != 0)
        .or_else(|| games.first());
    json!({"runtime":"Monado", "activity":if current.is_some(){"other"}else{"idle"},
        "app":current.map(|c| c.1.as_str()).unwrap_or(""),
        "clients":clients.iter().map(|(id,name,flags)| json!({"id":id,"name":name,"flags":flags})).collect::<Vec<_>>()})
}

#[cfg(target_os = "linux")]
struct Monado {
    lib: libloading::Library,
    root: *mut std::ffi::c_void,
}
#[cfg(target_os = "linux")]
impl Monado {
    unsafe fn open() -> Result<Self, ()> {
        let mut candidates = Vec::new();
        if let Ok(path) = std::env::var("PRIM_LIBMONADO") {
            candidates.push(path);
        }
        candidates.extend([
            "libmonado.so".into(),
            "libmonado.so.25".into(),
            "/run/current-system/sw/lib/libmonado.so".into(),
        ]);
        let lib = candidates
            .iter()
            .find_map(|p| libloading::Library::new(p).ok())
            .ok_or(())?;
        let version = lib
            .get::<unsafe extern "C" fn(*mut u32, *mut u32, *mut u32)>(b"mnd_api_get_version\0")
            .map_err(|_| ())?;
        let (mut major, mut minor, mut patch) = (0, 0, 0);
        version(&mut major, &mut minor, &mut patch);
        if major != 1 {
            return Err(());
        }
        let create = lib
            .get::<unsafe extern "C" fn(*mut *mut std::ffi::c_void) -> i32>(b"mnd_root_create\0")
            .map_err(|_| ())?;
        // Verify destructor before acquiring a connection.
        lib.get::<unsafe extern "C" fn(*mut *mut std::ffi::c_void)>(b"mnd_root_destroy\0")
            .map_err(|_| ())?;
        let mut root = std::ptr::null_mut();
        if create(&mut root) < 0 || root.is_null() {
            return Err(());
        }
        Ok(Self { lib, root })
    }
    unsafe fn poll(&mut self) -> Result<Vec<(u32, String, u32)>, ()> {
        type Root = *mut std::ffi::c_void;
        let update = self
            .lib
            .get::<unsafe extern "C" fn(Root) -> i32>(b"mnd_root_update_client_list\0")
            .map_err(|_| ())?;
        let count_fn = self
            .lib
            .get::<unsafe extern "C" fn(Root, *mut u32) -> i32>(b"mnd_root_get_number_clients\0")
            .map_err(|_| ())?;
        let id_fn = self
            .lib
            .get::<unsafe extern "C" fn(Root, u32, *mut u32) -> i32>(
                b"mnd_root_get_client_id_at_index\0",
            )
            .map_err(|_| ())?;
        let state_fn = self
            .lib
            .get::<unsafe extern "C" fn(Root, u32, *mut u32) -> i32>(b"mnd_root_get_client_state\0")
            .map_err(|_| ())?;
        let name_fn = self
            .lib
            .get::<unsafe extern "C" fn(Root, u32, *mut *const std::ffi::c_char) -> i32>(
                b"mnd_root_get_client_name\0",
            )
            .map_err(|_| ())?;
        let mut count = 0;
        if update(self.root) < 0 || count_fn(self.root, &mut count) < 0 || count > 4096 {
            return Err(());
        }
        let mut clients = Vec::new();
        for i in 0..count {
            let (mut id, mut flags) = (0, 0);
            let mut name = std::ptr::null();
            if id_fn(self.root, i, &mut id) < 0
                || state_fn(self.root, id, &mut flags) < 0
                || name_fn(self.root, id, &mut name) < 0
            {
                return Err(());
            }
            if name.is_null() {
                return Err(());
            }
            let name = std::ffi::CStr::from_ptr(name)
                .to_string_lossy()
                .into_owned();
            clients.push((id, name, flags));
        }
        Ok(clients)
    }
}
#[cfg(target_os = "linux")]
impl Drop for Monado {
    fn drop(&mut self) {
        unsafe {
            if let Ok(destroy) = self
                .lib
                .get::<unsafe extern "C" fn(*mut *mut std::ffi::c_void)>(b"mnd_root_destroy\0")
            {
                destroy(&mut self.root);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overlays_do_not_own_the_room() {
        assert_eq!(
            classify_clients(&[(1, "WayVR".into(), 30), (2, "Prim".into(), 30)])["activity"],
            "idle"
        );
    }
    #[test]
    fn unfocused_game_still_blocks_prim_actions() {
        let s = classify_clients(&[(1, "WayVR".into(), 30), (2, "VTOL".into(), 2)]);
        assert_eq!(s["activity"], "other");
        assert_eq!(s["app"], "VTOL");
    }
    #[test]
    fn primary_game_wins_over_secondary_session() {
        assert_eq!(
            classify_clients(&[(1, "Waiting".into(), 2), (2, "VTOL".into(), 15)])["app"],
            "VTOL"
        );
    }
}
