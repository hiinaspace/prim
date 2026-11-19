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
  a change is a `voice_control(mic, deaf, tray_live)` request to the running voice client
  (one function, `Control::set_voice_state`): nothing restarts, the incarnation stays, peers'
  receivers do not reset. Voice is started once (`--start-muted`, `--start-deaf`, `--tray-live`
  as needed) and restarted only when `voice_status`/`voice_control` says `NotRunning`;
  `voice_reconnecting` means wait and retry. Exit 4 (takeover or policy) is not fought: the
  state becomes `busy` until the person turns voice on again (which takes the lease over).
  **Fail-safe:** every start the session makes (first start, relaunch after a crash, a session
  restart) is `--start-muted`; only an explicit request opens the mic, so a crashed voice
  never comes back live. `status.voice.mic/deaf` are what voice last reported, never what was
  asked for; an unacknowledged request stays pending (retried every 250 ms, `detail` says so).
  A live process is never replaced for a toggle (so no new incarnation). The session never
  talks to the voice client while holding its state lock. Open decision: `prim ctl session
  mic on` with no window and no tray is honoured (an explicit request; refusing needs the
  session to know a front end is attached, which only `tray_live` says today).
  The voice client mutes itself if Godot's tracking lease goes away and `tray_live` is false;
  the session reads that back (about once a second) so `status.voice.mic` is what voice does.
- **Status**: `{pid, room: none|starting|up|failed, kind: found|join|resume|attached,
  error, voice: {running, wanted, mic, deaf, state, detail, pid, starts}}`.
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
