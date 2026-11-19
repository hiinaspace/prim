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
     `-- prim voice       kept running; reconnects itself on lease loss
```

- **Sockets** sit beside `room.sock` (`$PRIM_SOCKET`, else `$XDG_RUNTIME_DIR/prim/`):
  `session.sock` (mode 0600). Logs: `$XDG_STATE_HOME/prim/session/{session,room,voice}.log`.
- **Single instance**: a second `prim session` finds the first and exits; every client
  calls `ensure` (start one if none answers, else use it). The children are spawned in their
  own process group, so a session that is killed leaves them running, and the next session
  **adopts** the live daemon and the live voice client (mic/deafen are read from its `voice_status` report)
  without restarting either.
- **Requests** (`{"op": ...}`): `status`, `create{name,args}`, `join{token,args}`,
  `resume{room,args}`, `leave`, `cancel`, `voice{on}`, `set_mic{on}`, `set_deafen{on}`,
  `quit`. The control surface is "set mic on/off, set deafen on/off, read the state":
  a change writes the **desired** state (`gen` bumped) and wakes the supervisor, the only thread
  that talks to the voice client; it always sends the *full* desired state (`voice_control(mic,
  deaf, tray_live)`), so requests cannot overtake each other. The call waits up to 3 s for voice's
  acknowledgement; `status.voice.mic/deaf` are what voice last reported, never what was asked for,
  and `voice.pending` is "still applying". Nothing restarts: same process, same incarnation, peers'
  receivers do not reset. A toggle with no voice process only records the desired state; it never
  starts voice or takes the lease (only `voice on` and the room coming up do). If voice mutes itself
  (no tracker, no tray) that becomes the desired state, so the session does not unmute it again.
  **Fail-safe:** every start the session makes (first start, relaunch, session restart) is
  `--start-muted` with `--start-deaf` following the desired deafen; only an explicit request opens
  the mic. Exit 4 (takeover or policy) is not fought: state `busy` until the person turns voice on
  (which takes the lease over). The session never talks to the voice client while holding its
  state lock. (The room-daemon calls for leave/cancel/quit in `daemon.rs` still run under it;
  they are short and not changed here.) Open decision: `prim ctl session mic on` with no window and
  no tray is honoured (an explicit request); refusing needs a front-end-attached signal, which only
  `tray_live` gives and it is false until T1.
  The voice client mutes itself if Godot's tracking lease goes away and `tray_live` is false;
  the session reads that back (about once a second) so `status.voice.mic` is what voice does.
- **Status**: `{pid, room: none|starting|up|failed, kind: found|join|resume|attached,
  error, voice: {running, wanted, mic, deaf, state, detail, pid, starts, pending}}`.
- **Joining**: `join` returns once the daemon is up; whether the room answers is read from
  `status.join` (ROOM.md). The session never gives up by itself; `cancel` does.
- **Voice** starts muted and hearing when a room comes up. "Voice on" while another program
  holds the lease takes it over.
- **`tray_live`** in the status is false until T1. A front end that is asked to close while it is
  false must leave the room and quit (no live microphone with nothing visible); Godot does
  this, unless `PRIM_CLOSE_KEEPS_SESSION=1` or `--keep-session` is given.
- **Tray**: `crates/prim-session/src/tray.rs` is the place (a stub); it gets a `Control`.
- **Test mode**: `PRIM_SESSION_VOICE_TEST=1` gives voice a tone source and a null sink.

The client-side model (status, names, chat on channel 100, the pending-request table, the
default-name rule) lives in `prim-client` (`room::Room`/`Handle`, `chat`), so Godot
(`PrimClient`), `prim ctl` and the dev window share one implementation.
