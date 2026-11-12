# prim-voice

The voice client of a `prim-room` daemon: `prim voice` (see `docs/room/VOICE.md`).

It holds the `voice` lease on the daemon's realtime socket, encodes 20 ms Opus packets from a
capture source and sends them, and renders everyone else's voice (NetEq per sender, then
prim-hrtf at that sender's seat) to an output.

```
prim voice --capture wav:talk.wav --output wav:out.wav --stats stats.json
prim voice --capture tone:auto --output null --stats stats.json --duration 30
prim voice track --rate 90 --sweep 30
```

`prim voice --help` lists the options. Cargo feature `device` (default) enables cpal audio devices;
without it only `wav:`, `tone:` and `null` sources and sinks exist.

## Data and credits

`data/cipic_124.phrtf` is CIPIC subject 124 baked by [prim-hrtf](https://github.com/hiinaspace/prim-hrtf)
(`data/cipic_124.phrtf` there, rev `e38fa7d`). It contains HRTF measurements from the CIPIC HRTF
database, University of California, and prim-hrtf's evaluator is ported from Steam Audio 4.8.1
(Valve, Apache-2.0). See prim-hrtf's `NOTICE` and `LICENSE` for the terms; they apply to this file too.

## Seats (R0)

Positions are in the listener's own seat frame (origin at the listener, -Z toward the table centre,
heads 1.6 m up). Everyone, listener included, sits at an even angle around a 1.5 m table in sorted-key
order; `--seat KEYPREFIX=DEG` and `--seat-index N=DEG` override with a fixed azimuth at 1.5 m. The
tracking source's pose (also in this frame) moves the listener; with no pose the listener sits still.
