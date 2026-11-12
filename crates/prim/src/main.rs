//! `prim`: a multi-call binary. `prim room ...` runs the room daemon, `prim ctl ...`
//! talks to its local socket, `prim voice ...` is the voice client. Invoked as
//! `prim-room`, `prim-ctl` or `prim-voice` it dispatches on its own name.

mod ctl;
mod room;
mod voice;

use std::process::ExitCode;

fn usage() -> ExitCode {
    eprintln!(
        "usage: prim room found|join <token>|resume <room-hex> [options]\n       \
         prim ctl [--socket PATH] <status|invite|grant|revoke|retire|close|reopen|leave|write|sub|send|listen> ...\n\
         prim voice [track] [options]\n\
         run `prim room --help`, `prim ctl --help` or `prim voice --help` for the options"
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().collect();
    let prog = std::path::Path::new(&args[0])
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let which = match prog.as_str() {
        "prim-room" => Some("room".to_string()),
        "prim-ctl" => Some("ctl".to_string()),
        "prim-voice" => Some("voice".to_string()),
        _ => None,
    };
    let (which, rest): (String, Vec<String>) = match which {
        Some(w) => (w, args.drain(1..).collect()),
        None => {
            if args.len() < 2 {
                return usage();
            }
            (args[1].clone(), args[2..].to_vec())
        }
    };
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = match which.as_str() {
        "room" => rt.block_on(room::run(rest)),
        "ctl" => rt.block_on(ctl::run(rest)),
        "voice" => voice::run(rest),
        _ => return usage(),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("prim {which}: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Tiny argument scanner shared by both subcommands: flags with values and booleans
/// are pulled out, the rest are positionals.
pub struct Args {
    pub positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

impl Args {
    /// `valued` lists the flags that take a value; every other `--x` is a boolean.
    pub fn parse(args: Vec<String>, valued: &[&str]) -> anyhow::Result<Args> {
        let mut positional = Vec::new();
        let mut flags = Vec::new();
        let mut it = args.into_iter();
        while let Some(a) = it.next() {
            if let Some(name) = a.strip_prefix("--") {
                if let Some((n, v)) = name.split_once('=') {
                    flags.push((n.to_string(), Some(v.to_string())));
                } else if valued.contains(&name) {
                    let v = it
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--{name} needs a value"))?;
                    flags.push((name.to_string(), Some(v)));
                } else {
                    flags.push((name.to_string(), None));
                }
            } else {
                positional.push(a);
            }
        }
        Ok(Args { positional, flags })
    }

    pub fn has(&self, name: &str) -> bool {
        self.flags.iter().any(|(n, _)| n == name)
    }

    pub fn value(&self, name: &str) -> Option<&str> {
        self.flags
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .and_then(|(_, v)| v.as_deref())
    }

    pub fn values(&self, name: &str) -> Vec<&str> {
        self.flags
            .iter()
            .filter(|(n, _)| n == name)
            .filter_map(|(_, v)| v.as_deref())
            .collect()
    }
}
