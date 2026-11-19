# cowatch-loop: the one-command room

`tools/cowatch-loop` puts you in a private Prim room with a bot peer: two
`prim room` daemons (A found, B join), a bot writing a moving
`member/<id>/pose` live record, and the Godot room (`client/`) attached to
daemon A. Scene files under `client/room/` hot-reload while it runs.

    tools/cowatch-loop --desktop       # window on this machine
    tools/cowatch-loop --vr            # system OpenXR runtime (headset)
    tools/cowatch-loop --verify [--desktop] [--fault views1|kill-bot-daemon]

`--verify` runs headless oracles (roster has 2 members, bot pose arrives,
per-eye capture non-black with render views == scene views in VR, marker moves
and follows the pose) and exits non-zero on any failure. VR verify uses the
xrprobe harness (private Monado, null compositor); set `XRPROBE` if it is not
at `~/code/xrprobe`. Gates: Xid baseline/re-check, VRAM >= 8 GiB, refuses
`--vr` while another client holds the Monado socket (`--allow-busy`).

Each run writes `~/.local/state/prim-loop/runs/<stamp>/` including
`events.jsonl` (spawn, daemon_ready, first_frame, joined, ...; see
`crates/prim-events`) and the cold-start table is printed at the end.
`first_voice` hooks belong in prim-voice where the first decoded frame
reaches the sink; not wired in L0.

`crates/prim-client` is the Rust client for room.sock; its `Transport` trait
is frozen at the end of L0. `native/src/prim_client.rs` exposes it to Godot.
