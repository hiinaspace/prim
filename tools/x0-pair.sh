#!/usr/bin/env bash
# X0 sensitivity run: instance A (VR, per-eye crops on private Monado) and instance B (desktop window) play the
# block-clock video; B is released OFFSET seconds after A (negative = earlier). One GPU-lock hold for both.
# usage: x0-pair.sh LABEL OFFSET_S [LEAD_S=90]
set -uo pipefail
label=$1; off=$2; lead=${3:-90}
tools=$(dirname "$(realpath "$0")")
start=$(python3 -c "import time; print(time.time()+$lead)")
startB=$(python3 -c "print($start+($off))")
echo "pair $label offset=$off start=$start"
flock -o /tmp/prim-sayu-gpu.lock bash -c '
  export X0_NOLOCK=1 X0_NOTHUMB=1 X0_MULT=4 X0_SECONDS=10 X0_BURST=20
  "$0"/x0-run.sh "$1"A vr "$2" &
  a=$!
  "$0"/x0-run.sh "$1"B desktop "$3" &
  b=$!
  wait $a $b
' "$tools" "$label" "$start" "$startB"

[ -f /tmp/x0-xdg/weston.pid ] && kill "$(cat /tmp/x0-xdg/weston.pid)" 2>/dev/null; rm -f /tmp/x0-xdg/weston.pid
