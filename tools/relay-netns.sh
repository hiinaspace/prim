#!/usr/bin/env bash
# Run a command in an unprivileged pasta netns whose firewall leaves only the public relay path:
# UDP is dropped (except DNS) and LAN addresses are unreachable, so direct/holepunched paths cannot form.
# Usage: relay-netns.sh CMD ARGS...   (inside: needs iptables; outside: nix-shell -p passt)
if [ -z "$PRIM_IN_NETNS" ]; then
  export PRIM_IN_NETNS=1
  exec nix-shell -p passt --run "pasta --config-net -- $(printf '%q ' "$0" "$@")"
fi
iptables -A OUTPUT -o lo -j ACCEPT
iptables -A OUTPUT -p udp --dport 53 -j ACCEPT
iptables -A OUTPUT -d 192.168.0.0/16 -j DROP
iptables -A OUTPUT -d 10.0.0.0/8 -j DROP
iptables -A OUTPUT -p udp -j DROP
exec "$@"
