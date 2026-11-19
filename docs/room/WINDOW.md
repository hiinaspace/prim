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

## Advanced view

A read-only tree of what this daemon knows: room (lifecycle, generation, coordinator
and term, `room/*` keys), members (flags, every `member/<id>/*` key with value, class,
writer, version and age), channels, leases and holders, per-peer connections (route,
RTT, datagram counters) and the realtime lane's drop counters. "Age" is the time since
this computer last saw the value change; records carry no wall-clock time.

## Local API gaps found

- No request to stop the daemon (found by scanning `/proc`).
- No runtime mic mute or deafen in `prim voice` (the window replaces the client).
- The registered-channel table is not in `Status`, so the advanced view lists only the
  window's own channel.
- A room has no name the API can read (`found --meta` is stored but not exposed), so the
  window shows the first hex digits of the room id.
- A daemon serves one room, so "leave and join another" means stop and start a daemon.
