# The session (round A1, 2026-11-19)

`prim session` is the one process that owns "being in a room" on this machine. Godot,
`prim ctl`, the dev window (`prim ui`) and a later tray only send it requests, so closing
or killing any of them changes nothing: the room daemon, voice and your identity keep
going. Property: kill Godot mid-call and your friends keep hearing you; reopen it and you
are the same person in the same room with the same mute state
(`tools/test-session-survive.py`).

```
Godot / prim ctl / prim ui / tray     (clients; any may come and go)
        |  session.sock  (one JSON request, one reply per connection)
   prim session           supervisor: single instance, own process group
     |-- prim room        daemon, one room (room.sock, rt.sock)
     `-- prim voice       kept running, relaunched on lease loss
```

- **Sockets** sit beside `room.sock` (`$PRIM_SOCKET`, else `$XDG_RUNTIME_DIR/prim/`):
  `session.sock` (mode 0600). Logs: `$XDG_STATE_HOME/prim/session/{session,room,voice}.log`.
- **Single instance**: a second `prim session` finds the first and exits; every client
  calls `ensure` (start one if none answers, else use it). The children are spawned in their
  own process group, so a session that is killed leaves them running, and the next session
  **adopts** the live daemon and the live voice client (mic/deafen are read from its argv)
  without restarting either.
- **Requests** (`{"op": ...}`): `status`, `create{name,args}`, `join{token,args}`,
  `resume{room,args}`, `leave`, `cancel`, `voice{on}`, `set_mic{on}`, `set_deafen{on}`,
  `quit`. The control surface is "set mic on/off, set deafen on/off, read the state":
  today a change replaces the voice client with `--takeover` (one function,
  `Control::set_voice_state`); round VC changes its body to a request to the running client.
- **Status**: `{pid, room: none|starting|up|failed, kind: found|join|resume|attached,
  error, voice: {running, wanted, mic, deaf, state, detail, pid, starts}}`.
- **Joining**: `join` returns once the daemon is up; whether the room answers is read from
  `status.join` (ROOM.md). The session never gives up by itself; `cancel` does.
- **Voice** starts muted and hearing when a room comes up. "Voice on" while another program
  holds the lease takes it over.
- **Tray**: `crates/prim-session/src/tray.rs` is the place (a stub); it gets a `Control`.
- **Test mode**: `PRIM_SESSION_VOICE_TEST=1` gives voice a tone source and a null sink.

The client-side model (status, names, chat on channel 100, the pending-request table, the
default-name rule) lives in `prim-client` (`room::Room`/`Handle`, `chat`), so Godot
(`PrimClient`), `prim ctl` and the dev window share one implementation.
