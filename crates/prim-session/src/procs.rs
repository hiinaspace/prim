//! Spawning detached children and finding the ones an earlier session left behind.

use std::fs::File;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use prim_client::paths;

fn log_file(name: &str) -> std::io::Result<File> {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths::log_dir().join(name))?;
    use std::io::Write;
    let _ = writeln!(f, "--- {:?} ---", std::time::SystemTime::now());
    Ok(f)
}

pub fn last_log_line(name: &str) -> String {
    std::fs::read_to_string(paths::log_dir().join(name))
        .ok()
        .and_then(|s| {
            s.lines()
                .rev()
                .find(|l| !l.trim().is_empty() && !l.starts_with("---"))
                .map(str::to_string)
        })
        .unwrap_or_default()
}

/// Run `prim <args>` in its own process group with output to `log`: closing or killing the
/// session does not touch it.
pub fn spawn(args: &[String], log: &str) -> std::io::Result<Child> {
    let mut c = Command::new(paths::prim_exe());
    c.args(args)
        .stdin(Stdio::null())
        .stdout(log_file(log)?)
        .stderr(log_file(log)?);
    // SAFETY: setsid is async-signal-safe; own session so nothing that happens to the
    // launcher (its terminal, scope or process group) reaches this process.
    unsafe {
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    c.spawn()
}

pub fn terminate(pid: i32) {
    // SAFETY: plain signal to a pid this process spawned or verified from /proc.
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
}

pub fn pid_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks existence.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// `prim <sub> ...` programs whose `--<flag>` equals one of `want` (a flag that is absent
/// matches only when `default_ok`). Returns `(pid, argv)`.
pub fn find(sub: &str, flag: &str, want: &[PathBuf], default_ok: bool) -> Vec<(i32, Vec<String>)> {
    let me = std::process::id() as i32;
    let our_name = paths::prim_exe()
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut out = vec![];
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return out;
    };
    for e in rd.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else {
            continue;
        };
        if pid == me {
            continue;
        }
        let Ok(raw) = std::fs::read(e.path().join("cmdline")) else {
            continue;
        };
        let argv: Vec<String> = raw
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect();
        let name = argv
            .first()
            .and_then(|a| Path::new(a).file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name != our_name || argv.get(1).map(String::as_str) != Some(sub) {
            continue;
        }
        let given = flag_value(&argv, flag);
        let matches = match &given {
            Some(v) => want.iter().any(|w| w == Path::new(v)),
            None => default_ok,
        };
        if matches {
            out.push((pid, argv));
        }
    }
    out
}

pub fn flag_value(argv: &[String], flag: &str) -> Option<String> {
    let f = format!("--{flag}");
    argv.iter().enumerate().find_map(|(i, a)| {
        if let Some(v) = a.strip_prefix(&format!("{f}=")) {
            Some(v.to_string())
        } else if *a == f {
            argv.get(i + 1).cloned()
        } else {
            None
        }
    })
}
