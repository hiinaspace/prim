# Voice: `prim-voice` as a client of `prim-room` (round R0, 2026-11-11)

Wire formats are in [CONTRACTS.md](CONTRACTS.md) sections 5, 6 and 8. This file is
the local side: the realtime socket, leases, queue bounds, clocks, and how
`prim-voice` is built. The local API is provisional while every client ships in the
one `prim` binary (`prim room|voice|ctl`).

## Path of one packet

```
mic ─► prim-voice (encode 20 ms Opus) ─rt.sock─► prim-room A ─prim/1 datagram─►
       prim-room B (stamp received_at_mono_us) ─rt.sock─► prim-voice
       (fence → NetEq → prim-hrtf at the sender's seat → mix) ─► output device
```

Two IPC crossings per direction, one network hop. The daemon owns the single
`prim/1` connection per peer; voice never opens its own.

## The realtime socket

- `$XDG_RUNTIME_DIR/prim/rt.sock` (next to `room.sock`, same 0700 directory),
  `SOCK_SEQPACKET`: one message per packet, connection-scoped, so a crash is a
  close. Codec: `room_core::rt::RtMsg`, ≤ 2048 B.
- It carries only the lease handshake, voice frames and listener poses. Control,
  state, snapshots and messages stay on `room.sock`, so nothing on the realtime
  socket waits behind them (head-of-line blocking is per socket).
- The daemon's realtime work runs on its own thread (a dedicated current-thread
  runtime), not on the room driver, so a snapshot flood or a busy core can't delay
  it. Voice datagrams take a **fast path**: the transport receive task stamps
  `received_at_mono_us` and hands kind 2/3 datagrams straight to the realtime lane,
  which checks the sender against a membership view the driver publishes. They never
  enter the room core.
- Windows will use an overlapped named pipe in message mode behind the same trait.

## Leases

A lease is a role held by one realtime-socket session. Roles: `voice` and
`tracking_source` (exclusive per daemon). Later: `presentation`, `player`.

- **Acquire.** The first message is `Hello{role, takeover, incarnation, flags}`. If
  the role is free, the daemon answers `Welcome{self_key, lease_ttl_ms, session}`.
  If it is held and `takeover = 0`, it answers `Refused{busy}` and closes, so a
  second voice launch fails visibly. With `takeover = 1` the old session gets
  `Revoked{takeover}` and is closed, and the new one gets the lease.
- **Liveness.** Any message renews the lease. Clients send `Keepalive` every 250 ms
  when idle. The lease ends when the socket closes or nothing arrives for
  `lease_ttl_ms` (2000 ms): `Revoked{expired}`, then close. A SIGSTOPped client
  therefore loses its lease in 2 s.
- **On voice lease start, change or end** the daemon writes `member/<id>/voice`
  (`listening`, `mic`, `incarnation`); on end, `flags = 0, incarnation = 0`.
- **No lease, no voice.** Without a `voice` holder the daemon drops arriving voice
  datagrams (counted `voice_no_lease`) and sends none.
- **Session fencing.** On a new session (restart or takeover) the daemon discards
  everything queued for or from the old session: the inbound queue and any queued
  outbound datagrams whose incarnation isn't the new session's. A `VoiceOut` whose
  incarnation differs from the session's is dropped (`voice_wrong_incarnation`);
  `SetState` changes it explicitly.

## Queue bounds (size **and** age)

| queue | bound by size | bound by age | on overflow |
|---|---|---|---|
| daemon → voice client (`VoiceIn`) | 256 messages, ≤ 32 per sender | 100 ms since `received_at` | drop oldest; counted per reason |
| voice client → network (per peer datagram queue) | the driver's datagram queue (128) | `max_age_ms` 40 | drop; counted |
| socket buffers | `SO_SNDBUF` 64 KiB each way | — | `EAGAIN` keeps the message in the user-space queue, where the age bound applies |
| `prim-voice` receive | NetEq's own buffer | `VoiceIn` older than 100 ms at read is dropped | counted |
| listener pose | latest value only | — | overwritten |

All drop counters are in `prim ctl status` (daemon) and the `prim-voice` stats JSON.

## Clocks

- One local clock domain: `mono_us` = CLOCK_MONOTONIC µs (`clock_gettime`, not a
  per-process `Instant` epoch). The daemon stamps `received_at_mono_us` at datagram
  receive. `prim-voice` hands that stamp to NetEq as the packet's arrival time, so
  NetEq measures network jitter, not IPC jitter. The IPC delay is
  `read_mono_us − received_at_mono_us`, recorded as a histogram.
- A stamp in the future or over 1 s old means the domains don't match. `prim-voice`
  then uses its own read time and counts `clock_mismatch`.
- Sender and receiver sample clocks differ by device drift. NetEq absorbs that by
  time-stretching; the fault matrix runs talkers at ±500 ppm.

## Listener at the source rate

The `tracking_source` holder sends `PoseOut{source_mono_us, seq, pose}` at its own
rate (90 Hz in tests). The daemon forwards each one to the voice holder at once as
`Listener{source_mono_us, forwarded_mono_us, …}`. It is never coalesced to the 20 Hz
network pose timer. `prim-voice` uses the newest pose at each render block and
records `listener_age = render_mono_us − source_mono_us`.

## `prim-voice`

Threads; the device callbacks only copy between preallocated ring buffers:

- **rt reader:** decodes `VoiceIn`, fences it by incarnation per sender
  (`room_core::voice::IncarnationFence`), resets that sender's receiver on a new
  stream, pushes to the sender's `voice_core::VoiceReceiver` (NetEq) with the
  daemon's arrival stamp. Stores the newest `Listener`.
- **mixer:** paced by the output ring's fill (two 10 ms blocks ahead). Per block,
  per active sender: pull 480 samples from NetEq, render with `prim_hrtf::Renderer`
  from the sender's seat relative to the listener pose, gain
  `prim_hrtf::voice_gain(distance) × level_match`, sum. `level_match` is −4.7 dB:
  prim-hrtf runs that much louder than the Godot/Steam Audio path at equal gain
  (R2a), so the two match.
- **output:** the device callback (cpal/ALSA → PipeWire), a WAV sink paced by the
  wall clock for headless runs, or null.
- **capture:** the device callback, or a WAV file paced by a clock (synthetic
  talkers, with an optional ppm skew). An encoder thread makes 20 ms Opus packets
  (`voice_core::VoiceEncoder`) and sends `VoiceOut`.
- **Seats are fixed** in R0: seat angles around a table of radius 1.5 m, assigned by
  sorted member key, or given explicitly with `--seat KEY=AZIMUTH`.
- **Device change:** the stream is reopened on the current default device; the
  device generation increments, the capture ring is flushed (stale mic samples are
  discarded), the encoder advances its timestamp over the gap, and the incarnation
  stays.
- **Restart:** a new process draws a new incarnation. Nothing of the old stream
  survives it: its daemon queues are discarded (session fencing) and receivers fence
  its late packets.

## Oracles (R0)

Run on a Linux desktop with real PipeWire devices; histograms land in a CACHEDIR.TAG directory.

1. Seven synthetic talkers, each `prim room` + `prim voice --capture wav:…`, and one
   listener (`prim room` + `prim voice` on the real output device) over real iroh on
   loopback: every stream decodes, concealment and underruns are counted.
2. IPC delay histogram per hop under bulk load (a reliable-message flood, a state
   write flood and CPU load). Pass: p99.9 < 2 ms per hop; p99 of both hops together
   < 10 ms (one playout block).
3. Listener-age histogram at `prim-voice`, tracker at 90 Hz. Pass: p99 < one source
   interval + 4 ms.
4. Fault matrix: daemon pause (SIGSTOP 500 ms, both ends), voice crash and restart
   (talker and listener), device change, mismatched clocks (local domain offset;
   ±500 ppm talkers).
5. Stale-buffer disposal: each incarnation of a talker sends a distinct tone; after a
   restart the listener's output contains none of the old tone.
