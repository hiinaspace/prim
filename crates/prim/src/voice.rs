//! `prim voice`: the voice client, and `prim voice track`, the pose source for the
//! listener-age oracle. All the work is in `prim-voice`.

use std::process::ExitCode;

use anyhow::Result;
use prim_voice::Flags;

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
    let args = Args::parse(args, prim_voice::VALUED_FLAGS)?;
    prim_voice::run(&args)
}
