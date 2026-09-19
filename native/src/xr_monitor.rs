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
            let mut monado: Option<Monado> = None;
            #[cfg(target_os = "linux")]
            let mut retry_at = Instant::now();
            #[cfg(target_os = "linux")]
            let mut last_selection = String::new();
            #[cfg(target_os = "linux")]
            let mut status_error = String::new();
            while run.load(Ordering::Relaxed) {
                let state = configured_runtime();
                #[cfg(target_os = "linux")]
                let mut state = state;
                #[cfg(target_os = "linux")]
                {
                    let selected = state["runtime"] == "Monado" || active.load(Ordering::Relaxed);
                    // A socket may be socket-activated. Do not connect merely because it exists.
                    let service_up = std::fs::read_dir("/proc").is_ok_and(|entries| entries.flatten().any(|entry| {
                        std::fs::read_to_string(entry.path().join("comm")).is_ok_and(|s| s.trim() == "monado-service")
                    }));
                    if selected && (service_up || active.load(Ordering::Relaxed)) {
                        let selection = state["library"].to_string();
                        if selection != last_selection {
                            monado = None; retry_at = Instant::now(); last_selection = selection;
                        }
                        if monado.is_none() && Instant::now() >= retry_at {
                            retry_at = Instant::now() + Duration::from_secs(15);
                            match unsafe { Monado::open(&state) } {
                                Ok(client) => { status_error.clear(); monado = Some(client); }
                                Err(message) => { if status_error != message { eprintln!("PRIM_XR_MONITOR {message}"); } status_error = message; }
                            }
                        }
                        if let Some(client) = monado.as_mut() {
                            match unsafe { client.poll() } {
                                Ok(clients) => {
                                    let mut observed = classify_clients(&clients);
                                    observed["manifest"] = state["manifest"].clone();
                                    observed["library"] = state["library"].clone();
                                    observed["status_library"] = json!(client.path);
                                    state = observed;
                                }
                                Err(_) => { monado = None; status_error = "Monado status connection lost; explicit reveal remains available".into(); }
                            }
                        }
                        if monado.is_none() { state["activity"] = json!("unknown"); state["detail"] = json!(status_error); }
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
    fn configured_runtime_json(&self) -> GString {
        GString::from(configured_runtime().to_string().as_str())
    }
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
        #[cfg(target_os = "windows")]
        if let Some(path) = windows_runtime_path() {
            paths.push(path);
        }
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
                let manifest = std::fs::canonicalize(&path).unwrap_or(path.clone());
                let raw = value["runtime"]["library_path"].as_str().unwrap_or("");
                let resolved = resolve_runtime_library(&manifest, raw);
                return json!({"runtime":name, "activity":"unknown", "manifest":manifest, "library":resolved});
            }
        }
    }
    json!({"runtime":"Undetected", "activity":"unknown"})
}

fn resolve_runtime_library(manifest: &std::path::Path, library: &str) -> std::path::PathBuf {
    let path = std::path::Path::new(library);
    let resolved = if path.is_absolute() { path.to_owned() }
        else { manifest.parent().unwrap_or(std::path::Path::new(".")).join(path) };
    std::fs::canonicalize(&resolved).unwrap_or(resolved)
}

#[cfg(target_os = "linux")]
fn monado_candidates(runtime: &Value) -> Vec<String> {
    let mut paths = Vec::new();
    if let Ok(path) = std::env::var("PRIM_LIBMONADO") { paths.push(path); }
    if let Some(library) = runtime["library"].as_str() {
        if let Some(directory) = std::path::Path::new(library).parent() {
            for name in ["libmonado.so", "libmonado.so.25"] {
                paths.push(directory.join(name).to_string_lossy().into_owned());
            }
        }
    }
    paths.extend(["libmonado.so".into(), "libmonado.so.25".into(), "/run/current-system/sw/lib/libmonado.so".into()]);
    paths.dedup();
    paths
}

#[cfg(target_os = "windows")]
fn windows_runtime_path() -> Option<std::path::PathBuf> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStringExt;
    #[link(name = "advapi32")]
    extern "system" {
        fn RegGetValueW(key: *mut c_void, subkey: *const u16, value: *const u16,
            flags: u32, kind: *mut u32, data: *mut c_void, size: *mut u32) -> i32;
    }
    let subkey: Vec<u16> = "SOFTWARE\\Khronos\\OpenXR\\1\0".encode_utf16().collect();
    let value: Vec<u16> = "ActiveRuntime\0".encode_utf16().collect();
    let mut data = vec![0u16; 4096];
    let mut size = (data.len() * 2) as u32;
    // HKLM, REG_SZ, explicitly use the 64-bit runtime registration.
    let result = unsafe { RegGetValueW(0x80000002u32 as i32 as isize as *mut c_void,
        subkey.as_ptr(), value.as_ptr(), 0x00010002, std::ptr::null_mut(),
        data.as_mut_ptr().cast(), &mut size) };
    if result != 0 { return None; }
    let end = data.iter().position(|c| *c == 0)?;
    Some(std::ffi::OsString::from_wide(&data[..end]).into())
}

#[cfg(any(target_os = "linux", test))]
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
    path: String,
    lib: libloading::Library,
    root: *mut std::ffi::c_void,
}
#[cfg(target_os = "linux")]
impl Monado {
    unsafe fn open(runtime: &Value) -> Result<Self, String> {
        let mut failed = Vec::new();
        for path in monado_candidates(runtime) {
            if let Ok(mut client) = Self::open_one(&path) {
                if client.poll().is_ok() {
                    eprintln!("PRIM_XR_MONITOR connected status_library={path}");
                    return Ok(client);
                }
            }
            failed.push(path);
        }
        Err(format!("No compatible Monado status client (load/API/IPC probe failed): {}. Explicit reveal remains available.", failed.join(", ")))
    }
    unsafe fn open_one(path: &str) -> Result<Self, ()> {
        let lib = libloading::Library::new(path).map_err(|_| ())?;
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
        Ok(Self { path: path.to_owned(), lib, root })
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
