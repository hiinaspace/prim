#!/usr/bin/env bash
# X0: one instance of the block-clock oracle, with a private null sink recorded on the side.
# usage: x0-run.sh LABEL vr|desktop [START_UNIX] [VIDEO]      env: X0_RECT X0_SECONDS X0_BURST
# VR mode: private Monado + headless weston via xrprobe-run, under the shared GPU lock.
# Desktop mode: Godot --xr-mode off on a private headless weston (x0-wl), no Monado.
# Outputs go to ~/x0-work/runs/LABEL (CACHEDIR.TAG at ~/x0-work).
set -uo pipefail
label=$1; mode=$2; start=${3:-0}; video=${4:-$HOME/x0-work/video/bc30.mp4}
prim=~/code/prim-x0
out=~/x0-work/runs/$label; mkdir -p $out
export MONADO=${MONADO:-$HOME/code/xrprobe/.local/monado-5b133708}
profile=$(mktemp -d /tmp/x0-profile.XXXXXX)
sink=x0sink-$label
mod=$(pactl load-module module-null-sink sink_name=$sink sink_properties=device.description=x0-null)
before=$(journalctl -k -b | grep -c Xid)
tools=$prim/tools
(cd $tools && uv run blockclock_audio.py record $sink $out/audio) &
rec=$!
sleep 1
envs=(LIBMPV_ZERO_MPV_LIBRARY=$prim/.local/mpv/lib/libmpv.so.2 PRIM_NETWORK_LOCAL_ONLY=1 PRIM_XR_SCENE=1 PRIM_MEDIA=$video
      XDG_DATA_HOME=$profile PULSE_SERVER=unix:/run/user/$(id -u)/pulse/native PULSE_SINK=$sink
      X0_MODE=$mode X0_OUT=$out X0_ROLE=$label X0_START_UNIX=$start X0_RECT=${X0_RECT:-0.28,0.38,0.44,0.32}
      X0_SECONDS=${X0_SECONDS:-12} X0_BURST=${X0_BURST:-30})
if [ "$mode" = vr ]; then
  args=(); for e in "${envs[@]}"; do args+=(--env "$e"); done
  cd ~/code/xrprobe
  lock=/tmp/prim-sayu-gpu.lock; [ "${X0_NOLOCK:-}" = 1 ] && lock=/dev/null  # pair runs hold the lock in x0-pair.sh
  flock $lock bash -c 'for i in $(seq 40); do ss -ltn | grep -q ":4242 " || break; sleep 3; done; exec "$@"' _ \
    uv run xrprobe-run --project $prim/project --godot $prim/.local/godot/bin/godot --timeout 240 --out $out/xrprobe "${args[@]}" \
    -- --audio-driver PulseAudio --script res://tests/xrprobe_blockclock.gd -- --desktop > $out/run.log 2>&1
else
  export XDG_RUNTIME_DIR=/tmp/x0-xdg WAYLAND_DISPLAY=x0-wl
  mkdir -p $XDG_RUNTIME_DIR; chmod 700 $XDG_RUNTIME_DIR
  if [ ! -S $XDG_RUNTIME_DIR/$WAYLAND_DISPLAY ]; then
    ~/code/xrprobe/.local/weston/bin/weston --idle-time=0 --backend=headless --renderer=gl --socket=$WAYLAND_DISPLAY --width=1280 --height=720 >$out/weston.log 2>&1 9>&- &
    echo $! > $XDG_RUNTIME_DIR/weston.pid
    sleep 3
  fi
  env "${envs[@]}" PULSE_SERVER=unix:/run/user/$(id -u)/pulse/native $prim/.local/godot/bin/godot --path $prim/project \
    --display-driver wayland --audio-driver PulseAudio --xr-mode off --max-fps 60 --resolution 1280x720 --script res://tests/xrprobe_blockclock.gd -- --desktop > $out/run.log 2>&1
fi
rc=$?
sleep 1; kill -TERM $rec 2>/dev/null; wait $rec 2>/dev/null
pactl unload-module $mod
rm -rf $profile
echo "exit=$rc xid_before=$before xid_after=$(journalctl -k -b | grep -c Xid)" >> $out/run.log
tail -3 $out/run.log
