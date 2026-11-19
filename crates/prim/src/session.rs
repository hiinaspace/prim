//! `prim session`: the supervisor (crate `prim-session`).

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;

use crate::Args;

const HELP: &str = "\
prim session [--socket PATH] [--no-voice]

Runs the supervisor: starts and keeps `prim room` and `prim voice`, and answers
create/join/leave/voice requests on session.sock (beside the room socket). A second
`prim session` finds the first and exits. Drive it with `prim ctl session ...`.

  --socket PATH   the room daemon's local socket (default $PRIM_SOCKET, else
                  $XDG_RUNTIME_DIR/prim/room.sock)
  --no-voice      do not start voice by itself when a room comes up
";

pub fn run(args: Vec<String>) -> Result<ExitCode> {
    let args = Args::parse(args, &["socket"])?;
    if args.has("help") {
        eprint!("{HELP}");
        return Ok(ExitCode::from(2));
    }
    prim_session::run(prim_session::Options {
        room_socket: args
            .value("socket")
            .map(PathBuf::from)
            .unwrap_or_else(prim_client::paths::room_socket),
        voice: !args.has("no-voice"),
    })?;
    Ok(ExitCode::SUCCESS)
}
