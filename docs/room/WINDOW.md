# The room window (round GW, 2026-11-18)

`prim ui` opens a small window for the room daemon: who is here, what each person
has running, voice on/off, text chat, and an advanced view of the live room. It is a
client of `room.sock` like `prim ctl`. The daemon knows nothing about it.

```
prim ui            the window (egui; Linux for now, like the daemon)
prim ctl chat send TEXT...      post a chat line (scripts, agents)
prim ctl chat tail [--count N] [--timeout S]
```

## What the window does

- **No daemon running:** it offers *Create room* and *Join* (paste an invite). Both
  start `prim room found|join` for you; *Rejoin* resumes a room from its saved state.
  If a daemon is already on the socket (`$PRIM_SOCKET`, else the default) it attaches.
- **In a room:** a participant list (online state, route and RTT, voice listening and
  mic, 3D room, player), the launch controls, chat, and an *Advanced view*.
- **Closing the window** keeps the daemon, the room and any voice client running.
  *Leave* leaves the room and stops the daemon; *Quit Prim* does that and exits. The
  local API has no shutdown request, so the window finds the daemon process in `/proc`
  and sends SIGTERM.
- **Voice** is `prim voice` started by the window. The mic starts muted. The voice
  client has no runtime mute, so *Mic* and *Deafen* replace the client (`--takeover`,
  a new incarnation). The window relaunches it when it exits with 4 (lease expired),
  at most 5 times in 30 s.
- **Launch table:** `CLIENTS` in `crates/prim-ui/src/app.rs`. A new client is one
  entry (label, blurb, icon, and either the program to run or an `unavailable` reason).

## Chat: app channel 100

Chat is an ordinary app message channel. Nothing in the daemon knows about it.

- Channel **100**, reliable message to `all`. A sender also sends one to its own key so
  that other local clients on its daemon (a window, `chat tail`) see it; a sender that
  keeps its own log (the window) does not.
- Payload: `[version u8 = 1][UTF-8 text]`, text at most 2000 bytes, trimmed, not empty.
  Unknown versions and bad UTF-8 are dropped.
- Sender is the message's `from` key; its display name is `member/<from>/profile`.
- Not persistent and no history: a late joiner sees only what is sent after it joins.
  Only current members receive it (the daemon acks `not_member` otherwise).
- Channels 1-99 are left for first-party use.

## App state keys the window writes

| key | writer | value |
|---|---|---|
| `member/<id>/profile` | each member, persistent | `{"v":1,"name":"..."}`, name at most 32 bytes. Defaults to the machine's hostname the first time. Other clients (the 3D room, round C1) should write and read the same key. |
| `member/<id>/clients` | each member, presence-bound | `{"v":1,"godot":bool,"player":bool,"deaf":bool}`: which of the member's programs run, beyond `member/<id>/voice` (CONTRACTS.md section 6), which says listening and mic. `deaf` exists because the voice key has no such bit. |

## Audio settings (round GW2)

The **Audio** button (top bar) and **Audio settings** (Voice chat row) open a panel: pick the
microphone and speakers (or "System default"), a live mic meter (RMS bar, peak tick, CLIP
mark), input gain, output volume, and **Test microphone**: record 5 s, then play it back
through the speakers after gain, Opus (16 kbps, DTX) and the receive path (NetEq).

- Settings are per computer, in `$XDG_CONFIG_HOME/prim/audio.json` (`PRIM_AUDIO_SETTINGS`
  overrides), read by `prim voice` itself, so every way of starting voice uses them. Gain and
  volume changes apply to a running voice within 0.5 s; a device change restarts voice, as mute does.
- A saved device that is not connected falls back to the system default and says so in the
  panel and in the voice log; the choice is kept and returns when the device does.
- Nothing is sent to other members and `rt.sock` is unchanged. The window never opens a
  device: `prim voice devices` lists, `prim voice meter` publishes the level (a 32-byte file,
  rewritten ~30 Hz) while the panel is open and voice is off, `prim voice test` runs the
  loopback (JSON progress lines on stdout), and a running `prim voice --levels PATH`
  publishes the level itself. Helpers die with the window; voice does not.
- Differs from what a friend hears: no spatialisation, no network delay, jitter or loss, and
  your own mic is monitored locally.
- Where processing would go: `Audio::condition_input` in `crates/prim-voice/src/audio.rs`
  (after gain, before the meter and the encoder).
- Test hooks: `PRIM_UI_DEMO=audio` / `audiotest`, `PRIM_UI_AUDIO_DEVICES=<json>` (fixed list),
  `PRIM_UI_AUDIO_TONE=<hz>` (the private tone source; 0 is silence), with `PRIM_UI_VOICE_TEST=1`.
  `crates/prim-ui/tools/audio-shots.py` takes the five screenshots.

## Advanced view

A read-only tree of what this daemon knows: room (lifecycle, generation, coordinator
and term, `room/*` keys), members (flags, every `member/<id>/*` key with value, class,
writer, version and age), channels, leases and holders, per-peer connections (route,
RTT, datagram counters) and the realtime lane's drop counters. "Age" is the time since
this computer last saw the value change; records carry no wall-clock time.

## Local API gaps found

- ~~No request to stop the daemon~~ (A1: `shutdown`, see ROOM.md).
- No runtime mic mute or deafen in `prim voice` (the window replaces the client).
- The registered-channel table is not in `Status`, so the advanced view lists only the
  window's own channel.
- ~~A room has no name the API can read~~ (A1: `status.name`, see ROOM.md).
- A daemon serves one room, so "leave and join another" means stop and start a daemon.
