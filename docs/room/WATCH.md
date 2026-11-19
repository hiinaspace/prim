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

## Starting, restarting and changing the source (rule for any host, P2 included)

A host drives `prim_cowatch::Cowatch` and must follow these rules.

1. **One machine per process, one `Load` per intent, no respawn to change the source.**
   To change what the room watches, send `UserCommand::Load { kind, source }` to the
   *running* machine, from any member. Do not restart the player process for it. `prim watch`
   does this when the person loads something in mpv itself (drag and drop, `loadfile`): it
   sees mpv's `path` change to something it did not load and issues the `Load`.
2. **Send `Event::Synced` after the first snapshot of all three subscriptions**
   (`room/playback`, `member/*/playback`, `member/*/cowatch`; i.e. after the last
   `SnapshotDone`). Until then the machine holds user commands (at most 32; released after
   `Config::sync_wait_ms`, 3 s, if the host never says). A host may issue `Load` right after
   `Cowatch::new`, as `prim watch URL` does; it just waits for `Synced`.
3. **Why:** a restarted process has the same member key as the run before it, and that run's
   records (`room/playback` if it coordinated, `member/<me>/playback`) are still in the room
   until the snapshot arrives. A `Load` applied against an empty view wrote revision 1; the
   old run's higher revision then arrived, counted as newer, and the old URL played again
   (S's "entering a new URL just opened the previous one"). Defence in depth: if a record by
   our own key with a revision this process never wrote displaces ours anyway, the machine
   writes our latest again above it; and request ids start from the start time so the
   authority's dedupe cannot swallow a restarted guest's first request.
4. **Do not feed player reports for a source the machine did not load.** After the person
   loads something in the player, report `Loading` (or nothing) until the machine's
   `PlayerCmd::Load` for it comes back, or the machine seeks the new file to the old place.
   `prim watch` reports `Loading` for up to 5 s.

## Known limits

- Pressing play after the end does nothing: mpv re-pauses at the end of the file.
- Two members with no route to each other each act as leader if the coordinator runs no
  player (seen in a test with no discovery; real rooms have routes).
- YouTube needs a working yt-dlp for the machine's network; not verified end to end here.
