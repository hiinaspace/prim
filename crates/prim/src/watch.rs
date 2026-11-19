//! `prim watch`: the standalone mpv player that cowatches through the room daemon.
//! All the work is in `prim-watch`.

use std::process::ExitCode;

use anyhow::Result;
use prim_watch::Flags;

use crate::Args;

impl Flags for Args {
    fn positional(&self) -> &[String] {
        &self.positional
    }
    fn has(&self, name: &str) -> bool {
        Args::has(self, name)
    }
    fn value(&self, name: &str) -> Option<&str> {
        Args::value(self, name)
    }
    fn values(&self, name: &str) -> Vec<&str> {
        Args::values(self, name)
    }
}

pub fn run(args: Vec<String>) -> Result<ExitCode> {
    let args = Args::parse(args, prim_watch::VALUED_FLAGS)?;
    prim_watch::run(&args)
}
