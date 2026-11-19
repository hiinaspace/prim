# Block-clock sync oracle (round X0)

Reads "which video moment is on screen" from pixels, and "when did the audio tick play" from a private null sink,
so sync checks don't depend on the player's own reported position.

| File | Job |
|---|---|
| `tools/blockclock.py OUT.mp4 --fps 30\|60` | Generates the test video: frame number as a Gray-coded 6x5 grid in a white border; audio tick each second (1 kHz) and each 10 s (2 kHz). |
| `tools/blockclock_decode.py` | `decode_image(rgb)` -> `{ok, frame, why, margin}` (or "unreadable"); `find_ticks(samples, rate)`. |
| `tools/blockclock_audio.py record SINK PREFIX` / `ticks PREFIX` | Records `SINK.monitor` with wall-clock chunk stamps; tick onsets as unix times. Refuses non-test sinks. |
| `tools/x0_timing.py RUNDIR ROLE --fps N` | Picture zero (unix time of video 0) from decoded captures, audio zero from ticks, with error figures. |
| `tools/x0_pair.py`, `x0_sweep.py`, `x0_sheet.py`, `x0_analyze.py` | Two-instance delta, cell-size sweep on real captures, contact sheet, batch decode. |
| `tools/x0-run.sh LABEL vr\|desktop [START_UNIX] [VIDEO]`, `x0-pair.sh` | One instance (VR on private Monado, or desktop on private weston) with a null sink recorded; `x0-pair.sh` holds the GPU lock once and runs VR + desktop together. |
| `project/tests/xrprobe_blockclock.gd`, `eye_crop_probe.gd` | In-app driver and a CompositorEffect that copies a full-resolution crop of each eye's colour buffer, stamped at the render callback. |

Calling it from another round:
1. In each instance, capture the screen region of each eye every ~150 ms (`eye_crop_probe.gd`: `request(Rect2, tag)` then `take()`; keep `t_us` as the timestamp, convert to unix time with one `Time.get_unix_time_from_system()` / `get_ticks_usec()` pair taken together). Use native resolution, not the 64x64 thumbnail.
2. `decode_image()` each crop; keep captures that decode.
3. `x0_timing.pic_zero(rows, fps, eye)` gives that instance's video zero on the unix clock. Inter-instance drift = difference of zeros; use at least ~50 captures per instance.
4. A/V: `x0_timing.aud_zero()` on the sink recording; picture zero minus audio zero.
Do not run the decoder on the 64x64 thumbnail, and do not use desktop-window timestamps for sub-100 ms claims.
