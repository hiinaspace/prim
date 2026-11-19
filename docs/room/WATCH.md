# `prim watch`: cowatching in mpv

Round PM. A room client that runs the system `mpv` and keeps it on the same moment as
everyone else's, through the room daemon. Playback sync itself is `crates/prim-cowatch`
(sans-IO); this is its first real host. The Godot/libmpv client (P2) is the second.

```
prim watch [--socket PATH] [--mpv PATH] [--mpv-arg ARG]... [--ipc PATH] [URL-or-file]
```

With a URL or file it loads that for the room; without, it joins whatever the room is
watching. Pause, seek and mpv's own keys and on-screen controls move everyone. Close mpv
or press ctrl-C to leave (the room goes on). A file path means every member must have the
same file at that path; URLs go through mpv's yt-dlp support.

Room window: the "Watch together (mpv)" row has a link/file entry and starts and stops it
(`ClientKind::Watch`, supervised like voice, logs in `~/.local/state/prim/ui/watch.log`).

## Shape

- `crates/prim-watch/src/mpv.rs`: child mpv (`--idle --force-window --keep-open --pause`,
  `--input-ipc-server`), a reader thread, synchronous `get_property`.
- `player.rs`: mpv as a cowatch player. Every command we send leaves an expectation
  (our seeks, our pauses, the `start=` seek of a load); property changes and seeks that
  match none are the person's and become `UserCommand`s.
- `lib.rs`: the host loop. `prim-client` events in (state updates, messages on channel
  101, datagrams on 102, results), `Cowatch` actions out. A report and `Tick` every 100 ms.

Things the mpv event model forced (all in the adapter; `prim-cowatch` needed no change):

- `keep-open=yes`: end of file is a loaded file with `eof-reached`, not `end-file`, so a
  clean end and a dropped stream both arrive as `Ended` and the machine tells them apart
  by position against duration.
- mpv pauses itself at end of stream, also an early one, and may report `pause` before
  `eof-reached`. A pause is only the person's if `eof-reached` has not followed within
  250 ms. Without this a dropped stream paused the whole room forever.
- `loadfile ... start=N` makes mpv emit a `seek` event after the load: expected, not the
  person's.
- A person's seek is reported as seeking at its target until `playback-restart`; mpv's
  `time-pos` still shows the old place meanwhile, and the machine would seek them back.
- mpv reconnects a dropped HTTP stream itself (three tries) and shows a cache stall
  first; the room position runs on and the machine seeks past it when playback returns.

## Test hooks

`PRIM_WATCH_DEBUG=1` (stderr trace), `PRIM_WATCH_MPV_ARGS="--vo=null --ao=null"`,
`PRIM_WATCH_FAULT=no_correction` (correction loop off, to show an offset persists),
`PRIM_MPV`. Oracles: `crates/prim/tests/watch.rs` (real daemon processes on loopback, real
mpv with null video and audio); `crates/prim-ui/tools/watch-shots.py` (the window).

## Known limits

- Pressing play after the end does nothing: mpv re-pauses at the end of the file.
- A file or URL dragged into mpv's window is not shared; use the window's entry.
- Two members with no route to each other each act as leader if the coordinator runs no
  player (seen in a test with no discovery; real rooms have routes).
- YouTube needs a working yt-dlp for the machine's network; not verified end to end here.
