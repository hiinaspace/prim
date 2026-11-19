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
- **Reconnect inside one process (round VC).** Losing the lease to expiry or a closed
  socket does not end `prim voice`: it connects again with `Hello` carrying the **same
  incarnation** and its current mic/listening flags, backing off 50 ms, 100 ms ... 1 s.
  Because the incarnation is unchanged, the daemon's outbound fence keeps the stream and
  peers see no new stream (no receiver reset). The room key `member/<id>/voice` flickers to
  flags 0 for the instant between release and the new acquire. `Revoked{takeover}`,
  `Revoked{policy}` (3) and any unknown reason are final (exit 4). `Refused{busy}` while
  reconnecting is retried for 3 s, then exit 3. Media sent while the link is down is dropped
  and counted (`drop_no_link`). Without `rt_socket` in the config (library tests over a
  socket pair) a lost lease still ends the client as before.
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

## Runtime control (round VC)

Mute and deafen without restarting `prim voice`. The client owns the truth.

```
Godot PrimClient / prim ctl / supervisor --room.sock--> daemon --rt VoiceControl--> voice
voice --SetState (peers)--> daemon       voice --VoiceReport{ack,state}--> daemon --> the requester
```

- **Local API** (`room.sock`, JSON): `voice_control {req, mic?, deaf?, tray_live?}` (`mic: true`
  = microphone live) and `voice_status {req}`. Both answer `{"kind":"voice","voice":{voice_running,
  session, incarnation, listening, mic, muted, deaf, tray_live, reported, tracking_held}}`;
  errors are `voice_not_running`, `voice_no_ack` (no `VoiceReport` within 1 s) and `bad_request`
  (nothing to change). A request with no voice client is **not remembered**.
- **rt messages.** `VoiceControl{req u32 != 0, mic, deaf, tray_live}` (0x87, daemon to
  client; each field is one byte: 0 leave, 1 false, 2 true). `VoiceReport{ack u32, state u8}`
  (0x06, client to daemon; `state` bits: 1 muted, 2 deaf, 4 tray_live; `ack` is the request it
  answers, 0 for a change the client made itself). `TrackingLease{held}` (0x88, daemon to
  client; sent on voice acquire with the current state and on every tracking acquire/release).
  The client sends `SetState` (the `member/<id>/voice` flags) and then `VoiceReport`.
- **Mute** drops frames before the encoder; capture keeps running (so the OS microphone
  indicator stays on while muted). The gate cuts capture into 20 ms frames and reads the target
  at each frame boundary: the first muted frame fades out over 10 ms, then whole frames are
  dropped. The first muted frame ends the talkspurt (END marker, consumes a seq with the
  timestamp of that frame); every dropped frame advances only the timestamp by 960, so seq is
  contiguous and the timestamp counts the muted time. Unmuting fades in over 10 ms and the
  first packet carries START. The incarnation and the encoder are never replaced. Worst-case
  effect latency is one frame (20 ms) plus the 10 ms fade.
- **Deafen** ramps the final mix to zero over one 10 ms block. When fully deaf the mixer still
  drains inboxes and pulls each sender (cursors stay current) but renders no HRTF, and resets
  the sources once. The outgoing stream is untouched. The `listening` bit goes to 0, and the
  other daemons stop sending to a member that is not listening (their `listening()` check), so
  inbound traffic stops and returns only after the new flag has propagated: undeafening is
  audible after about one key propagation, not instantly.
- **Safety default.** On a *held to released* transition of the tracking lease (seen by voice,
  not on an initial "not held") the client mutes unless `tray_live` is set (`--tray-live` or
  `voice_control tray_live`). It keeps listening. A later explicit unmute is not undone.
- **Listener cleared.** On the same transition the stored head pose is dropped: the mixer falls
  back to the seat/origin pose (default head, identity rotation) at once. The daemon also drops
  its stale forwarded pose.
- **Start flags:** `--start-muted`, `--start-deaf`, `--tray-live`. A supervisor should start
  voice with `--capture device --output device --start-muted` and control it afterwards.
- **Acknowledgement.** The control thread sets the targets, waits up to 100 ms until the
  encoder and the mixer have each passed a frame boundary under the new version, then reports
  (`control_ack_timeouts` counts a miss). The rt reader never waits for it.
- **CLI:** `prim ctl voice mute|unmute|deafen|undeafen|tray-live|no-tray|status` prints the
  client's report as JSON (exit 1 with `voice not running` or `no ack`).
- **Oracles:** `tools/r0/oracles.py vc` (see "Oracles (R0)").

## `prim-voice`

Threads; the device callbacks only copy between preallocated ring buffers:

- **rt reader:** decodes `VoiceIn`, fences it by incarnation per sender
  (`room_core::voice::IncarnationFence`), resets that sender's receiver on a new
  stream, pushes to the sender's `voice_core::PlayoutReceiver` (see "Receiver" below)
  with both the daemon's arrival stamp and its own read time. Stores the newest
  `Listener`.
- **mixer:** paced by the output ring's fill (two 10 ms blocks ahead; more when the
  device pulls more at once, see "Ring depth" below). Per block, per sender that is not
  idle (see "Idle peers and stage timing"): pull 480 samples from the receiver, render with `prim_hrtf::Renderer`
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
- **Restart:** a new process draws a new incarnation (a lost lease inside a running process
  does not restart anything; see "Reconnect inside one process"). Nothing of the old stream
  survives it: its daemon queues are discarded (session fencing) and receivers fence
  its late packets.

## Receiver (timeline playout, 2026-11-18)

`voice_core::PlayoutReceiver` replaced the NetEq wrapper. One per sender, no clock of its
own (every time is a `u64` of microseconds the caller passes).

- **Timeline.** Fixed 20 ms slots on the packets' 48 kHz timestamps (+960 per slot, wrap-safe,
  `u16` seq and `u32` timestamp). A cursor says which slot plays next. Each slot is decoded,
  recovered from the next packet's in-band FEC (only if the packet carries it *and* the
  receiver config has `fec` on), or concealed with Opus PLC. A packet behind the cursor is
  counted `late` and never played; a repeat is `duplicate`; nothing plays after a newer
  timestamp. Seq only separates loss from DTX (a timestamp gap with contiguous seq is
  silence, not loss). Packets that are not exactly one 960-sample mono 48 kHz Opus frame are
  `Rejected` before any accounting.
- **Delay.** WebRTC-style: relative delay against a 2 s trailing minimum, a peak tracker that
  rises at once and decays 5 ms/s, target = peak + 10 ms clamped to 20..250 ms. The peak is
  bounded: a relative delay is clamped to `max - 2 x margin` (230 ms) before it can raise it, and
  one above `2 x max` (a stray timestamp, a path stalled far past what a buffer could cover) is not
  fed to it, so no single packet pins the target for longer than 46 s (it used to be 100 s for a
  packet 0.8 s late, and a timestamp 10 min ahead gave a peak of 600 s). A packet more than 64
  slots behind the cursor is dropped as late without reaching the estimator. The estimator
  sees the daemon stamp only; a late packet whose daemon-side delay was within the target is
  counted `local_late` (the miss was in this process). The buffer is held at
  `floor(target/20)+2` slots: a stretch needs 1.5 slots in hand, so the target counts the
  slot being played plus one.
- **Skip.** If the buffer holds more than `max/20 + 8` slots (400 ms at the defaults: six slots
  beyond the largest target, so Accelerate's fast mode at `target + 4` always gets its chance
  first) the receiver jumps to half the maximum delay behind the newest packet. The jump
  cross-fades one slot of the old timeline into the new one over 20 ms and starts the decoder
  cold; it was a hard splice, and fired at 290 ms, before Accelerate could act at the cap
  (`skipped_samples`).
- **Level control.** Accelerate / PreemptiveExpand (copied from the videocall-rs fork) on
  the 20 ms boundary only, never on concealed audio, not in the first 160 ms of a talkspurt.
  The search costs about 330 us per operation at 48 kHz in a release build (1.6 % of a core
  per peer if it ran every slot), so it is not downsampled. The fork's Accelerate is
  energy-based, not pitch-synchronous: in normal mode it only cuts inside a quiet stretch.
- **Talkspurts.** START/END are hints. A one-packet DTX refresh (a single packet followed by END
  or a DTX gap; Opus sends one every 400 ms in a quiet room) is *decoded* so the decoder keeps its
  state, but played as digital zeros: S's default is digital silence in pauses, and an isolated
  20 ms burst of room noise every 400 ms is a 2.5 Hz tick on headphones. It causes no target reset and
  no prebuffer (`dtx_refreshes`, `silence_samples`). A real talkspurt of exactly one packet
  followed by silence is muted the same way (a one-packet utterance is not a thing speech does; a
  start forced on one packet after the onset wait with the second packet over 40 ms late loses that
  one slot). A real onset sets the cursor to the first
  packet's timestamp and waits for the buffer to reach the target. Between talkspurts the
  output is digital zero and `is_idle()` is true. Concealment is bounded at 120 ms of
  consecutive lost slots, the last of them a fade-out; then the receiver goes idle and the next
  packet is a new onset. Wherever audio ends (END marker, DTX gap, the cap) one concealed frame is
  ramped to zero instead of cut (`fade_samples`; it was a step of 0.068 against the speech's own 0.032
  on the oracle signal). Time lost past the cap is `lost_unconcealed_samples` (zeros inside the
  timeline, plus the packets missing before a talkspurt that resumed after going idle on a loss,
  by `seq`); `silence_samples` is the sender's own pauses. A marker that reaches an idle receiver
  with no audio buffered is dropped, and onset wait counts only audio, not markers.
- **Opus.** `audiopus_sys` is patched to the in-repo `opus-sys` crate (a pinned libopus 1.5.2
  source tree built with cmake, linked statically), so desktop and the aarch64 shell use the
  same codec; `voice_core::opus_version()` is asserted in a unit test and printed as
  `opus_version` in the stats JSON.

### Idle peers and stage timing

Between talkspurts a receiver is idle (`is_idle()`), and the mixer stops rendering that peer
(`IdleGate` in `mixer.rs`): *Render* while it has audio, *Tail* for `ceil((interp_len-1)/480)`
more blocks of silent input so the HRTF's convolution tail leaves the filter, then *Park*
(history and gain reset by `reset_source`) and *Skip*. prim-hrtf's CIPIC filters are 200 taps and
`interp_len()` is a power of two (256), so the tail is one block; the unit test
`skipping_an_idle_peer_does_not_truncate_the_hrtf_tail` renders a noise burst with and without the skip; the
tail energy matches within 0.1 dB (identical to float noise: the tail block carries the whole residue).
A receiver that wakes (a new onset) leaves Skip on its first non-idle block. `--test-no-idle-skip`
turns the skip off for A/B runs only.

Per-stage histograms (microseconds, per block over all senders) are in the stats JSON under
`histograms`: `mixer_block_us`, `mixer_lock_us` (taking the per-peer lock), `mixer_pull_us`
(receiver pull incl. decode and level control), `mixer_hrtf_us`, `mixer_sum_us`. A `mixer`
object counts `peer_blocks_rendered`, `_tail` and `_skipped`. Per peer, `blocks_skipped`.
The receiver's concealment is real, not a proxy: `concealed_samples`, `late`, `reordered_accepted`,
`duplicate`, `local_late`, `dtx_refreshes` come from `PlayoutReceiver::stats()`.

### Ring depth

The device callback records the largest burst it was asked for (callbacks less than 2 ms apart
add up: ALSA delivers a 1024 quantum as 2 x 512) and the mixer keeps
`ceil(1.25 x burst / 480)` blocks rendered ahead, between 2 and 8. A fixed two blocks (960
frames) underran every other pull at quantum 1024 (1126 underruns in 38 s on the R0 build, 0 now).
Quantum 512 keeps R0's depth; 1024 keeps 3 blocks. Stats: `mixer.out_burst_frames`,
`mixer.ring_ahead_frames`. The burst is the maximum over the last 5-10 s (it used to be a running
maximum that one stall left high for the life of the process) and is cleared when the output
device is reopened. The standing playout latency is `floor(target/20)+2` slots: about
20 ms more than R0's NetEq at the same target.

### Measured (dev desktop, 2026-11-18, release, oracle-1 load: 7 talkers, speech clips, real PipeWire null sink)

| figure | value |
|---|---|
| listener age (daemon arrival to mixer output) | p50 5.6 ms, p99 11.0 ms, max 11.7 ms |
| buffer per talker (mean) | 48-67 ms, target 20 ms (`floor(20/20)+2` = 3 slots; R0's NetEq held ~40) |
| mixer block (all senders, 10 ms budget) | p50 0.26 ms, p99 1.1 ms, p99.9 1.7 ms, max 2.7 ms |
| stages at p50 / p99 | lock 0 / 2 us, pull 115 / 977 us, hrtf 114 / 243 us, sum 20 / 36 us |
| listener CPU | 6.0 % of one core (continuous talk); 4.8 % with 54 % of peer-blocks skipped vs 5.4 % without the skip (speech with pauses) |
| output underruns | 0 (default quantum and force-quantum 1024) |
| ring depth | 3 blocks (30 ms rendered ahead): the device pulls 1024 frames at once at the default quantum too |
| IPC (idle, 7 talkers) | in-queue p99.9 128 us, two hops p99 168 us |

The idle skip pays only with pauses: continuous talk skips 0.5 % of peer-blocks. Frame (aarch64) figures
are not in this table: the Frame was unreachable when this was written; the R0 figure (mixer max 26.7 ms)
is still the last Frame measurement.

### Test faults and exit

`prim voice` knobs for the harness, never set in normal use: `--test-drop-burst N,M` (drop M
of every N packets), `--test-reorder K,D` (hold every Kth packet back D packets),
`--test-no-idle-skip`, `--test-no-fade` (mute and unmute without the fade, so the click oracle can be shown to fail), and R0's `--test-drop-every`. They act on the talker after encoding, so
the receiver sees real network faults. A clean exit flushes held packets and sends the END
marker, so receivers go idle at once instead of concealing for 120 ms.

### Seeded-fault proof

The loss, ordering, DTX, wrap and drift oracles live in `voice-core/tests/`. Each was run
against the old receiver (`56b4c31`, NetEq) on the same scenarios, in real time because the old
receiver reads `Instant::now` itself (`docs/room/legacy-neteq-oracles.rs.txt` is the runner; it
needs the harness from `voice-core/src/fixture_harness.rs` as `tests/h.rs` in that checkout).
Real Opus encode of a 4 s synthetic voiced signal, 200 slots, 30 ms network delay.

| oracle | timeline receiver (pass criterion) | old NetEq receiver |
|---|---|---|
| loss every 50th (a) samples out vs 200 x 960 | exact | +1920 (two extra slots) |
| (c) concealed vs scripted 2 % | 2.00 % | 3.71 % (a 4-slot start-up concealment counted too) |
| loss every 20th (b) max step vs lossless | 0.0323 vs 0.0321 | 0.2692 vs 0.0573 (x4.70 splice) |
| (c) concealed vs scripted 5 % | 5.00 % | 7.18 % |
| bursts of 2-3: concealed vs 6 % | 6.00 % | 7.43 %; output +1920 |
| ordering (reorder, duplicate, 700 ms late) | dominant frequency never falls more than 75 Hz; 1 late, 1 duplicate, 960 samples concealed | chirp falls back 2725 Hz (old audio played after newer); `late_packets_discarded` 0; 3840 samples concealed; output +4800 |
| DTX refresh: target resets | 0, 2 prebuffers | 19 talkspurt flushes |
| seq + timestamp wrap | exact count, 960 concealed | debug-build panic, "attempt to multiply with overflow" in neteq `delay_manager.rs:118` |
| 60 s, +500 ppm, no DTX | 0 PLC, 0 late | 6240 samples concealed |
| 60 s, -500 ppm, no DTX | 0 PLC, 0 late | 4320 samples concealed; both runs kept 88 ms buffered |

The timeline column's continuity figure changed in the senior review. The lossless yardstick used
to include the end-of-stream cut (a step of 0.144 against the speech's own 0.032), which made
"within 1.5x lossless" a limit of 0.216 and let real splices pass. It is now measured on the
interior (first to last non-zero sample minus one slot): lossless 0.0321, every 50th 0.0321, every
20th 0.0323, bursts 0.0321, FEC and quantum-1024 runs inside 1.5x of the same. The old receiver's
column was not re-run; its own lossless figure (0.0573) is not smaller than an honest one, so its
0.2692 fails the limit by more than before (x4.7 against 0.0573, x8.4 against 0.0321).

The old receiver's numbers are not all failures of the same kind. Its per-loss concealment
count is near the scripted rate once the 3840-sample start-up concealment is subtracted, and
its continuity only breaks at 5 % loss. What it fails outright is ordering, DTX refreshes,
wrap, drift and the exact sample count. The step-delay and sparse-spike estimator tests were
not run against the old receiver.

### Reader/mixer hand-off (per-peer inbox)

The rt reader never takes a sender's `PeerRx` lock. It fences by incarnation and pushes into
that sender's `Inbox` (own tiny mutex, bounded at 256 entries); the mixer drains the inbox
into the `PlayoutReceiver` under the peer lock at the start of each pull. A new incarnation
clears the queue and puts a `Reset` first, so old-stream packets are never pushed into the new
stream. Measured in a full oracle run (bulk, 7 talkers): reader wait on the peer mutex
p99.9 1292 us (max 2.5 ms) before, 5 us (max 45 us) after; the mixer holds the lock p99.9
about 1.9 ms (max 3.6-5.5 ms) across decode and stretch, which is what the reader used to
wait behind. Bulk oracle 2a (hop p99.9 < 2 ms) still fails in full runs after this change, so
that lock was not the cause; see D-report.

### Known limits (timeline receiver)

Found by the senior review and deliberately not fixed yet:

- **Buffer after a small path-delay step is not brought back to the set point.** A +40 ms step
  (6 s into a 60 s stream, 0..8 ms jitter) leaves the buffer at a mean of 10 ms (min 0) for the
  next 44 s with 0 Preemptive Expand ops; +200 ms leaves it at 132 ms with 0 Accelerate ops.
  Root cause: both stretches need cleanly decoded audio for 1.5 slots in hand (`STRETCH_LOOKAHEAD`,
  30 ms), so once a step has drained the buffer under that, Preemptive Expand can never run,
  and the only thing that rebuilds depth is a talkspurt restart (an underrun, concealment, then
  the prebuffer to the raised target). It plays at zero margin until the next jitter peak. The
  fork's Accelerate is energy-based (cuts only in a quiet stretch), so on a continuously voiced
  signal it never lowers a buffer that is too deep either; real speech with pauses does. A fix
  needs a deliberate one-slot concealed insertion (a "hold") when the span is under the set point,
  which is audible and needs a listening pass, so it was not done blind.
- Resync after a timestamp jump (review item 6): a packet far ahead moves the anchor and the
  newest-packet mark; recovery relies on the skip and the late bound.
- Buffers are allocated while running (item 10), no preallocation.
- Time-stretch thresholds (item 11) are not tuned against real speech.
- `local_late` is not folded into the target (item 15).
- Items 14, 17-20 and 22 of the review are tracked in its report, not here.

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
6. **Loss** (`loss`): a steady 440 Hz tone, recorded from the private null sink's monitor, with
   the talker dropping every 50th, every 20th, and 2 of every 50 packets (2 %, 5 %, 4 %).
   Pass: exact sample accounting (`out + accelerate - preemptive == slots x 960`), receiver
   concealment within 0.35 points of the scripted rate, recorded duration at most 80 ms shorter than the receiver's
   output (the ring and sink still hold audio when the recorder stops; longer is a failure), and a continuous
   tone: max sample step at most 4x the tone's own (computed from the tone's amplitude), no run of
   near-silent samples longer than 48 (1 ms). A lossless run is the yardstick (L0). Opus PLC does not keep a
   pure tone's phase, so the phase hop and amplitude dip are reported, not asserted.
7. **Ordering** (`reorder`): swapped neighbours are accepted and counted `reordered_accepted`
   with no concealment and a continuous tone. A packet held 0.8 s is counted `late`, concealed
   once, never played, and `late + reordered_accepted` equals what the talker held (+-2). The
   late run must also be continuous and skip nothing (`skipped_samples` 0): 0.8 s is beyond twice
   the maximum delay, so the estimator ignores it and nothing is stretched. (Before the review
   this was waived on the grounds that Preemptive Expand splices the tone; the real cause was the
   skip firing before Accelerate could act.)
8. **Drift** (`drift`): two talkers at +-500 ppm with `--no-dtx`, 90 s: the target stays flat,
   no concealment, no late, no output underruns.
9. **Quantum 1024** (`quantum`): the oracle-1 load (7 talkers) with `clock.force-quantum 1024`
   and the listener at `PIPEWIRE_LATENCY=1024/48000`; the setting is restored afterwards.
   Pass: 0 underruns after warm-up, `mixer.out_burst_frames >= 1000` and the ring kept above it.

10. **Idle skip** (`idle`): talkers take turns (speech with pauses), skip on and off, then 7
   continuous talkers. Pass: >= 30 % of peer-blocks skipped, none with the skip off, lower
   listener CPU with the skip, no underruns in any.

Run one with `uv run --no-project python tools/r0/oracles.py loss|reorder|drift|quantum`, all with
`all`. `--listener-prim OLD` runs only the listener from another build (the harness's own
failing-on-old proof); `--godot DIR` points at the r2a-4 inputs when `~/code/prim-hrtf` is gone.
The oracles measure; they assume a quiet host. `L0` fails and says so when the clean recording
itself has a hole, which is what a loaded host does to the recorder.

Not built here: the Godot GDExtension (`gdext`) and `voice-mesh-bench` do not compile against the
current `voice-core` API (they still use the removed `VoiceReceiver` and `PacketArrival`, see `gdext/src/stream.rs`). Nothing in `prim` depends
on them.

### Runtime control (`oracles.py vc`, round VC)

WAV sinks, no PipeWire; the sink's start on CLOCK_MONOTONIC comes from the stats
(`control.out_t0_mono_us`). 1: 20 mic toggles in 10 s (the last four through `prim ctl`); silence
< -60 dB while muted, no click (max sample step <= 2x the plain tone's), one incarnation and one
listener-side new stream, `member/<id>/voice` mic bit within 200 ms, `ctl status` equals the client's
stats. 2: 20 deafens: silent < -60 dB, the other end hears the deaf client's tone with no gap,
listening bit within 200 ms, no HRTF blocks while deaf. 3: a relay between voice and the daemon cuts
the connection or delivers `Revoked{expired}` five times (client alive, back in <= 1.5 s, same
incarnation, deaf/tray-live re-applied, audio back); `Revoked{takeover}` and `{policy}` exit 4.
4: killing the tracker mutes (mic bit off within 200 ms, still listening) unless `--tray-live` or
`tray-live` was set. 5: killing the tracker brings the ear level difference back to the seat-pose
scene's. Each has a failing proof in the same run: `--test-no-fade` (click), restart-based mute,
deafen and reconnect (incarnation, listener streams), safety with `--tray-live`, and a tracker that
is not killed.
