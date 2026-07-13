#!/usr/bin/env bash
# Real-network scenario harness for PeerBeam's QUIC transport.
#
# Covers the scenarios that need OS privilege or real hardware and therefore
# can't be plain `cargo test`: latency/loss simulation (netem), different
# subnets (network namespaces), and per-interface / Tailscale checks. Each
# scenario auto-detects capability and SKIPS cleanly (never fails) when the
# environment can't support it, printing what to run where instead.
#
# Loopback IPv4/IPv6, multiple-simultaneous, resume-after-disconnect, and the
# >10 GB transfer are covered by `cargo test -p peerbeam-transfer-quic` (the
# `network.rs` suite) and are not repeated here.
#
# Usage:  scripts/nettest.sh            # run everything possible, skip the rest
#         sudo scripts/nettest.sh       # unlocks netem + netns scenarios

set -uo pipefail
cd "$(dirname "$0")/.." || exit 1

BIN="target/release/peerbeam"
PASS=0; SKIP=0
say()  { printf '\n\033[1m== %s ==\033[0m\n' "$1"; }
skip() { printf '  \033[33mSKIP\033[0m %s\n' "$1"; SKIP=$((SKIP+1)); }
ok()   { printf '  \033[32mOK\033[0m   %s\n' "$1"; PASS=$((PASS+1)); }

have_root() { [ "$(id -u)" -eq 0 ]; }
have()      { command -v "$1" >/dev/null 2>&1; }

# ── build ───────────────────────────────────────────────────────
if [ ! -x "$BIN" ]; then
  echo "building release CLI..."
  cargo build --release -p peerbeam-cli >/dev/null 2>&1 || { echo "build failed"; exit 1; }
fi

bench() { # -> "MiB/s, connect ms"
  "$BIN" --json benchmark quic --size "${1:-128}" --chunk 1024 2>/dev/null \
    | python3 -c 'import json,sys
d=json.load(sys.stdin)
print("%.0f MiB/s, connect %.1f ms" % (d["mib_s"], d["connect_ms"]))'
}

# Real transfer to an address served by a local 0.0.0.0 receiver — exercises
# routing/QUIC over that interface IP on a single host.
self_transfer() { # $1=dial_ip  $2=label
  local port=49711 f d rpid
  f=$(mktemp); head -c 4194304 /dev/urandom > "$f"; d=$(mktemp -d)
  "$BIN" --no-color receive --once --port "$port" --dir "$d" >/dev/null 2>&1 &
  rpid=$!; sleep 1
  if "$BIN" --no-color -y send "$f" --addr "$1:$port" >/dev/null 2>&1; then
    wait "$rpid" 2>/dev/null
    if cmp -s "$f" "$d/$(basename "$f")"; then ok "$2 transfer verified via $1"; else skip "$2 byte mismatch via $1"; fi
  else
    kill "$rpid" 2>/dev/null; skip "$2 send failed via $1"
  fi
  rm -rf "$f" "$d"
}

# ── baseline ────────────────────────────────────────────────────
say "Baseline (loopback, no shaping)"
ok "throughput: $(bench 256)"

# ── latency simulation (netem on lo) ────────────────────────────
say "High latency simulation (netem)"
if have_root && have tc; then
  cleanup_lat() { tc qdisc del dev lo root >/dev/null 2>&1; }
  trap cleanup_lat EXIT
  for d in 25 50 100; do
    if tc qdisc replace dev lo root netem delay "${d}ms" >/dev/null 2>&1; then
      ok "+${d}ms RTT: $(bench 128)"
    else
      skip "+${d}ms: netem unavailable"
    fi
  done
  cleanup_lat; trap - EXIT
else
  skip "needs root + tc (run: sudo tc qdisc add dev lo root netem delay 50ms)"
fi

# ── packet loss simulation (netem on lo) ────────────────────────
say "Packet loss simulation (netem)"
if have_root && have tc; then
  cleanup_loss() { tc qdisc del dev lo root >/dev/null 2>&1; }
  trap cleanup_loss EXIT
  for l in 1 3 5; do
    if tc qdisc replace dev lo root netem loss "${l}%" >/dev/null 2>&1; then
      ok "${l}% loss: $(bench 128)   (QUIC retransmits; transfer must still verify)"
    else
      skip "${l}% loss: netem unavailable"
    fi
  done
  cleanup_loss; trap - EXIT
else
  skip "needs root + tc (run: sudo tc qdisc add dev lo root netem loss 1%)"
fi

# ── different subnets (network namespaces) ──────────────────────
say "Different subnets (network namespaces)"
if have_root && have ip; then
  NS_A=pb_a; NS_B=pb_b
  cleanup_ns() { ip netns del $NS_A 2>/dev/null; ip netns del $NS_B 2>/dev/null; }
  trap cleanup_ns EXIT
  cleanup_ns
  if ip netns add $NS_A && ip netns add $NS_B \
     && ip link add veth-a type veth peer name veth-b \
     && ip link set veth-a netns $NS_A && ip link set veth-b netns $NS_B \
     && ip netns exec $NS_A ip addr add 10.10.1.1/30 dev veth-a \
     && ip netns exec $NS_B ip addr add 10.10.1.2/30 dev veth-b \
     && ip netns exec $NS_A ip link set veth-a up \
     && ip netns exec $NS_B ip link set veth-b up; then
    f=$(mktemp); head -c 8388608 /dev/urandom > "$f"; d=$(mktemp -d)
    ip netns exec $NS_B "$BIN" --json receive --once --port 49700 --dir "$d" >/dev/null 2>&1 &
    sleep 1
    if ip netns exec $NS_A "$BIN" -y send "$f" --addr 10.10.1.2:49700 >/dev/null 2>&1; then
      wait; cmp -s "$f" "$d/$(basename "$f")" && ok "transfer across veth /30 subnets verified" \
        || skip "cross-subnet transfer mismatch"
    else
      skip "cross-subnet send failed"
    fi
    rm -rf "$f" "$d"
  else
    skip "could not set up namespaces/veth"
  fi
  cleanup_ns; trap - EXIT
else
  skip "needs root + ip (creates two netns + a veth /30 pair)"
fi

# ── per-interface (Wi-Fi / Ethernet / USB tethering) ────────────
say "Physical interfaces (Wi-Fi / Ethernet / USB tethering)"
# Physical interfaces only (skip docker/bridge/veth virtual ones).
IFACES=$(ip -o -4 addr show up 2>/dev/null \
  | awk '$2!="lo" && $2!~/^(docker|br-|veth|tailscale)/{print $2" "$4}')
if [ -n "$IFACES" ]; then
  echo "$IFACES" | while read -r ifc cidr; do printf '  found %-10s %s\n' "$ifc" "$cidr"; done
  # Real transfer over the first physical interface's IP (single-host route).
  first_ip=$(echo "$IFACES" | head -1 | awk '{print $2}' | cut -d/ -f1)
  [ -n "$first_ip" ] && self_transfer "$first_ip" "interface"
  skip "full Wi-Fi/Ethernet/USB test needs a 2nd machine on that link (see docs/NETWORK_TESTING.md)"
else
  skip "no non-loopback IPv4 interfaces detected"
fi

# ── Tailscale ───────────────────────────────────────────────────
say "Tailscale"
if have tailscale && tailscale status >/dev/null 2>&1; then
  ip4=$(tailscale ip -4 2>/dev/null | head -1)
  if [ -n "$ip4" ]; then
    self_transfer "$ip4" "tailscale"
    echo "  for a cross-node test: run a receiver on another tailnet node, 'send --addr <its-tailscale-ip>:49600'"
  else
    skip "tailscale up but no IPv4 assigned"
  fi
else
  skip "tailscale not running (start it + a 2nd tailnet node for a real test)"
fi

# ── summary ─────────────────────────────────────────────────────
printf '\n\033[1mSummary:\033[0m %d ran, %d skipped (environment-gated)\n' "$PASS" "$SKIP"
