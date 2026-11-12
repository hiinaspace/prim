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
| voice client → daemon (`VoiceOut` in the realtime socket buffer) | `SO_SNDBUF` 64 KiB | `OUT_MAX_AGE_MS` 40 ms between the client's send stamp and the daemon's read (same host, same clock) | drop; counted `out_age`; the out_hop histogram records it first |
| datagrams arriving at the daemon | the ingress channel (1024); per-sender token bucket 100 pps / burst 50 and new streams 1 per s / burst 3 ("Rate limits") | **media-time lateness** over `IN_MAX_AGE_US` (100 ms), per sender, at enqueue, **only within a post-stall window** (below) | drop; counted `age_media` (outside the window: kept, counted `age_media_tolerated`); over a bucket: `rate` / `stream_rate` |
| daemon → voice client (`VoiceIn`) | 256 messages, ≤ 32 per sender | 100 ms since `received_at` | drop oldest; counted per reason |
| voice client → network (per peer datagram queue) | the driver's datagram queue (128) | `max_age_ms` 40 | drop; counted |
| socket buffers | `SO_SNDBUF` 64 KiB each way | — | `EAGAIN` keeps the message in the user-space queue, where the age bound applies |
| `prim-voice` receive | NetEq's own buffer | `VoiceIn` older than 100 ms at read is dropped; in a matched session (below) this holds for every stamp, so a stall replay is dropped | counted |
| capture ring (device callback → encoder) | 2 s of samples | **none.** After an encoder or process stall, stale mic audio is encoded with a fresh `sent_mono_us`, so `out_age` can't see it; the far daemon doesn't drop it unless it is itself recovering from a stall (it is a lateness step, `age_media_tolerated`) | overwrite oldest in the ring |
| iroh inbox pipe (transport → daemon, before the arrival stamp) | 64 KiB | **none**, and it sits before the stamp, so the stamp can't see its backlog; only the media-time bound does, and only if the whole daemon was stopped (see "Not covered") | `EAGAIN`/drop at the transport |
| `prim-voice` receive in *mismatch mode* | NetEq's own buffer | **none:** until a packet has been judged matched, the age bound doesn't apply (read time stands in) | counted `clock_mismatch` |
| listener pose | latest value only | — | overwritten |

**Why the read stamp alone can't bound age.** `received_at` is taken when the daemon
reads a datagram. While the daemon is stopped the kernel or the transport keeps the
datagrams; after it resumes they are all read, and stamped, within a few ms, so a
500 ms backlog looks fresh and NetEq would buffer it. The sender's 48 kHz sample
timestamp doesn't have that problem: per sender (reset when its incarnation changes)
the daemon keeps `offset = arrival - media_time` and `base`, the running minimum
(`base = min(offset, base + 1000 ppm of the wall time since the last packet)`, which
absorbs ±500 ppm drift). `offset - base` is how much later than usual a packet came.

**The lateness rule, gated.** `base` is tracked on every packet, but a packet with
`offset - base > IN_MAX_AGE_US` is **dropped only while the daemon is recovering from
a stall of its own**. Anywhere else the same lateness is a path-delay step (an iroh
relay fallback) or jitter above 100 ms, which NetEq absorbs better than a drop would:
the packet is kept, `base` follows it, and `age_media_tolerated` counts it so the steps
stay visible.

- *Stall detector.* The realtime lane looks at the clock at every event, and a 10 ms
  timer guarantees an event when it is idle. A gap over 50 ms between two looks means
  the thread was stopped (SIGSTOP, a blocked executor), counted `stalls` in `prim ctl
  status`. `on_ingress` looks first, so after SIGCONT whichever event runs first
  (backlog packet or timer) already knows. The lane is one thread, so there is no
  atomic: a separate heartbeat thread would miss a stall of the lane alone.
- *Window.* From the end of the stall for `100 ms + min(stall, 1 s)`. The backlog is
  read within milliseconds, but a transport that releases it in steps can take about as
  long as the stall; "the backlog has drained" can't be observed per sender, so the
  window is time-based and proportional. A second stall inside the window extends it.
- *Escape.* Inside the window, if a sender has had nothing but late packets for 200 ms
  of arrival time, `base` jumps to the current offset (a real pause backlog is read
  within milliseconds, so sustained lateness is a new path delay). A new incarnation
  resets the state.
- *Not covered.* A stall of only the transport's receive task or the iroh inbox pipe
  (not the lane) is not detected, and its backlog is then kept for NetEq to handle; a
  whole-process stop covers both. A sender that pauses doesn't release a backlog (its
  `out_age` bound drops it at the source).

Timestamps advance in real time through DTX and talkspurt gaps (the encoder counts
every captured 20 ms frame, sent or not, and polls until no full frame is pending, so
it can't fall behind the capture ring; a device change skips the gap in whole
frames), so silence is not mistaken for lateness. A capture xrun or process stall that
loses samples is not covered: the media clock then lags real time for good. That is
only a permanent `age_media_tolerated` step now, no longer a drop. The 32-bit timestamp
is unwrapped by signed distance from the previous one.

## Rate limits

Meant against buggy peers (a stuck loop, a flapping restart), not hostile ones. In the
daemon's inbound lane, per sender, after the datagram decodes and before the fence,
MediaAge or queues:

| bucket | sustained | burst | why | counted |
|---|---|---|---|---|
| voice datagrams | 100 pps | 50 | voice is 50 pps, so 2× with a second of burst; jitter and QUIC release bursts fit | `rate` |
| new streams (incarnation changes) | 1 per s | 3 | a restart per second is already a flapping client; each one resets the listener's NetEq under the per-peer mutex the mixer also takes | `stream_rate` |

Only kinds 2 and 3 pass through this lane (poses and everything else go through the
core), and kind 3 is decoded and dropped in R0, so it has no bucket. A new stream takes
its token *before* the fence moves; refused, the packet changes nothing (not the
fence, not MediaAge), so a stream refused now can start a second later and the client
never sees faster incarnation churn than this. That bound is why `prim-voice`'s
`recv.rs` needs no limit of its own: its NetEq resets happen only on `Fence::NewStream`
of a `VoiceIn`, and `VoiceIn` is produced only by the daemon's lane; nothing local
creates one. Inside a post-stall window the datagram bucket isn't charged: a long stop
releases a burst by construction and the age bound is its limiter (the per-sender and
total queue caps still apply). A voice client restarts at most about once per second in
the fault matrix, which the stream bucket's burst of 3 allows.

All drop counters (`age`, `age_media`, `age_media_tolerated`, `rate`, `stream_rate`, `out_age`, …) and the daemon's `stalls` are in `prim ctl status` and the `prim-voice` stats JSON; the harness tallies the four lane counters per scenario (`lane` oracle).

## Clocks

- One local clock domain: `mono_us` = CLOCK_MONOTONIC µs (`clock_gettime`, not a
  per-process `Instant` epoch). The daemon stamps `received_at_mono_us` at datagram
  receive. `prim-voice` hands that stamp to NetEq as the packet's arrival time, so
  NetEq measures network jitter, not IPC jitter. The IPC delay is
  `read_mono_us − received_at_mono_us`, recorded as a histogram.
- Match is decided once per rt session. Until a packet has been judged matched (stamp
  not in the future and at most 1 s old), a stamp in the future or over 1 s old means
  the domains don't match: `prim-voice` uses its own read time and counts
  `clock_mismatch`. Once any packet is matched the session is matched for good (the
  latch resets on reconnect), and a stamp over 100 ms old is `TooOld`, never a
  mismatch. So a 1.5 s stall of the voice client drops its backlog instead of
  replaying it.
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
- **Device change:** a stream error, SIGUSR1, or the watchdog reopens the stream. The watchdog (device manager thread) reopens when an open output or capture callback hasn't run for 300 ms (1.5 s if it never ran), counted `device_stall_reopen`; it restarts its baseline if its own thread was stopped. The reopen is in-process, so the routing env stays. The stream is reopened on the current default device; the
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
