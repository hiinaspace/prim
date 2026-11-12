#!/usr/bin/env bash
# End-to-end check of the room daemon over a real iroh path.
#
#   tools/room-netns.sh direct   two daemons on loopback, relay disabled, direct addresses
#   tools/room-netns.sh relay    B runs in tools/relay-netns.sh (UDP and LAN dropped), so only
#                                the public relay path can work; needs internet and nix-shell
#
# Builds `prim` (set PRIM_BIN to use a prebuilt binary), starts two daemons with isolated
# state dirs and sockets, founds a room on A, joins B with an invite token, writes member
# state on each side and reads it on the other, sends a message each way, then prints the
# selected path and max_datagram_size per peer (from `prim ctl status`). Any failed check
# exits nonzero. Scratch lives in a mktemp dir under /tmp and is removed on exit.
set -euo pipefail

mode=${1:-direct}
case "$mode" in direct | relay) ;; *) echo "usage: $0 [direct|relay]" >&2; exit 2 ;; esac
root=$(cd "$(dirname "$0")/.." && pwd)

if [ -n "${PRIM_BIN:-}" ]; then
  prim=$PRIM_BIN
else
  (cd "$root" && cargo build -p prim)
  prim=${CARGO_TARGET_DIR:-$root/target}/debug/prim
fi
[ -x "$prim" ] || { echo "no prim binary at $prim" >&2; exit 1; }

work=$(mktemp -d "${TMPDIR:-/tmp}/prim-room-netns.XXXXXX")
pids=()
cleanup() {
  local rc=$?
  for p in "${pids[@]:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
  if [ $rc -ne 0 ]; then
    echo "FAILED (exit $rc); logs kept in $work" >&2
    for f in a b; do echo "--- $f.log (tail)" >&2; tail -n 15 "$work/$f.log" >&2 || true; done
  else
    rm -rf "$work"
  fi
}
trap cleanup EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }
ctl_a() { "$prim" ctl --socket "$work/a.sock" "$@"; }
ctl_b() { "$prim" ctl --socket "$work/b.sock" "$@"; }

wait_ready() { # log file, label
  for _ in $(seq 1 300); do
    grep -q '^ready$' "$1" 2>/dev/null && return 0
    sleep 0.1
  done
  fail "$2 did not become ready"
}

if [ "$mode" = direct ]; then
  a_net=(--relay none --bind 127.0.0.1:0)
  b_net=(--relay none --bind 127.0.0.1:0)
  wrap=()
  settle=30
else
  a_net=(--relay n0)
  b_net=(--relay n0)
  wrap=("$root/tools/relay-netns.sh")
  settle=90
  # The netns copies the host's resolv.conf. A systemd-resolved stub (127.0.0.53) isn't
  # reachable from inside it, so point the daemon at a real upstream (UDP 53 is allowed).
  if [ -z "${PRIM_DNS:-}" ] && grep -q '^nameserver 127\.' /etc/resolv.conf; then
    for f in /run/systemd/resolve/resolv.conf; do
      [ -r "$f" ] && PRIM_DNS=$(awk '$1=="nameserver" && $2 !~ /:/ {print $2; exit}' "$f")
    done
  fi
  export PRIM_DNS="${PRIM_DNS:-}"
fi

"$prim" room found --state-dir "$work/a" --socket "$work/a.sock" "${a_net[@]}" >"$work/a.log" 2>&1 &
pids+=($!)
wait_ready "$work/a.log" A
key_a=$(awk '$1=="member"{print $2}' "$work/a.log")
listen_a=$(awk '$1=="listen" && $2 ~ /^127\./{print $2; exit}' "$work/a.log")
echo "A member ${key_a:0:12}  ${listen_a:+listen $listen_a}"

token=$(ctl_a invite --expiry 10m)
[ -n "$token" ] || fail "no invite token"

join_extra=()
if [ "$mode" = direct ]; then join_extra=(--peer-addr "$key_a=$listen_a"); fi
"${wrap[@]}" "$prim" room join "$token" --state-dir "$work/b" --socket "$work/b.sock" \
  "${b_net[@]}" "${join_extra[@]}" >"$work/b.log" 2>&1 &
pids+=($!)
wait_ready "$work/b.log" B
key_b=$(awk '$1=="member"{print $2}' "$work/b.log")
echo "B member ${key_b:0:12}"

# Both sides see a live connection to the other.
for _ in $(seq 1 "$settle"); do
  if ctl_a status | grep -q "^route peer=$key_b" && ctl_b status | grep -q "^route peer=$key_a"; then
    connected=1
    break
  fi
  sleep 1
done
[ -n "${connected:-}" ] || fail "A and B never connected"
echo "ok   connected"

# Member state, both directions.
ctl_a write member hello from-a --persistent >/dev/null
ctl_b write member hello from-b --persistent >/dev/null
ctl_b sub "member/$key_a/hello" --count 1 --timeout 30 | grep -q "text=from-a" || fail "B did not read A's state"
echo "ok   state A -> B"
ctl_a sub "member/$key_b/hello" --count 1 --timeout 30 | grep -q "text=from-b" || fail "A did not read B's state"
echo "ok   state B -> A"

# Messages, both directions.
for pair in "a b $key_b" "b a $key_a"; do
  set -- $pair
  from=$1 to=$2 target=$3
  ctl_$to listen 7 --count 1 --timeout 30 >"$work/listen-$to.out" &
  lp=$!
  sleep 1
  ctl_$from send "$target" 7 "ping-from-$from" --reliable | grep -q "status=delivered" || fail "message $from -> $to not delivered"
  wait "$lp" || fail "listener on $to got nothing"
  grep -q "text=ping-from-$from" "$work/listen-$to.out" || fail "message $from -> $to garbled"
  echo "ok   message $from -> $to"
done

echo "--- routes"
echo "A -> B: $(ctl_a status | grep "^route peer=$key_b")"
echo "B -> A: $(ctl_b status | grep "^route peer=$key_a")"
path_b=$(ctl_b status | sed -n "s/^route peer=$key_a path=\([a-z]*\) .*/\1/p")
path_a=$(ctl_a status | sed -n "s/^route peer=$key_b path=\([a-z]*\) .*/\1/p")
mds_b=$(ctl_b status | sed -n "s/^route peer=$key_a .*max_datagram_size=\([0-9a-z]*\) .*/\1/p")
echo "selected path: A=$path_a B=$path_b; max_datagram_size at B: $mds_b"
[[ "$mds_b" =~ ^[0-9]+$ ]] || fail "no max_datagram_size reported"
if [ "$mode" = direct ]; then
  [ "$path_a" = direct ] && [ "$path_b" = direct ] || fail "expected direct paths"
else
  [ "$path_b" = relay ] || fail "B is in a relay-only netns but reports path=$path_b"
fi
echo "PASSED ($mode)"
