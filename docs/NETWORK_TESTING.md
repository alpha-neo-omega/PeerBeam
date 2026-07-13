# Real-Network Integration Testing

PeerBeam's transport is exercised over **real QUIC endpoints** — real UDP
sockets, TLS 1.3, congestion control — not in-process mocks. Coverage is split
by what each scenario needs:

- **`cargo test` suite** (`crates/peerbeam-transfer-quic/tests/network.rs`) —
  everything that runs on one host with no special privilege: IPv4, IPv6,
  many-simultaneous, resume-after-disconnect, and a >10 GB transfer.
- **Harness** (`rust/scripts/nettest.sh`) — scenarios needing OS privilege
  (netem latency/loss, network-namespace subnets) or hardware (Wi-Fi, Ethernet,
  USB tethering, Tailscale, a second host). It auto-detects capability and
  **skips cleanly** where the environment can't support a scenario.

## How to run

```bash
# Automated suite (host loopback IPv4/IPv6, concurrent, resume)
cargo test -p peerbeam-transfer-quic --test network

# The >10 GB case (ignored by default; ~1 min, release recommended)
cargo test -p peerbeam-transfer-quic --release --test network -- --ignored large_file

# Privilege/hardware harness (unprivileged skips netem/netns; sudo unlocks them)
rust/scripts/nettest.sh
sudo rust/scripts/nettest.sh
```

## Scenario matrix

| Scenario | Where | How it's tested |
|---|---|---|
| **Same LAN** | harness | Transfer over a physical interface IP; full test = a 2nd machine on the LAN |
| **Different subnets** | harness (root) | Two network namespaces + a veth `/30` pair, transfer across them |
| **USB tethering** | manual | Bind to the `usb*`/`rndis` interface; send to a phone-tethered host |
| **Tailscale** | harness | Transfer over the `tailscale0` IP; cross-node = a 2nd tailnet node |
| **Wi-Fi** | harness | Transfer over the Wi-Fi interface IP; full test = a 2nd Wi-Fi host |
| **Ethernet** | manual | Same as Wi-Fi over the `eth*`/`enp*` interface |
| **IPv4** | cargo test | `ipv4_loopback_transfer` — byte-exact over `127.0.0.1` |
| **IPv6** | cargo test | `ipv6_loopback_transfer` — byte-exact over `::1` |
| **Large file (>10 GB)** | cargo test | `large_file_over_quic` — 11 GiB, constant memory (generator→sink) |
| **Resume after disconnect** | cargo test | `resume_after_real_disconnect` — link dropped mid-transfer, `send_file_recover` reconnects + resumes |
| **Multiple simultaneous** | cargo test | `multiple_simultaneous_transfers` — 8 concurrent QUIC transfers, each verified |
| **High latency** | harness (root) | `netem delay` on `lo`, sweep 25/50/100 ms |
| **Packet loss** | harness (root) | `netem loss` on `lo`, sweep 1/3/5 % (QUIC retransmits; must still verify) |

## Results (reference run)

Host: Intel i5-1135G7, Linux, single machine, unprivileged.

### Automated suite — all pass

| Test | Result |
|---|---|
| IPv4 loopback (2 MiB, byte-exact) | ✅ pass |
| IPv6 loopback `::1` (2 MiB, byte-exact) | ✅ pass |
| 8 simultaneous transfers (distinct payloads) | ✅ pass, all verified |
| Resume after real disconnect (reconnect confirmed) | ✅ pass |
| Large file **11 GiB** over real QUIC | ✅ pass — **36.3 s (~310 MiB/s)**, constant memory |

### Harness — this host

| Scenario | Result |
|---|---|
| Baseline loopback throughput | **~440 MiB/s, ~0.8 ms connect** |
| Wi-Fi interface (`192.168.1.8`) | ✅ transfer verified |
| Tailscale (`tailscale0`, `100.73.134.21`) | ✅ transfer verified |
| High latency (netem) | ⏭ skipped — needs root (`sudo` to run) |
| Packet loss (netem) | ⏭ skipped — needs root |
| Different subnets (netns) | ⏭ skipped — needs root |
| USB tethering / Ethernet / cross-node LAN | ⏭ manual — needs a 2nd machine / hardware |

> Loopback and single-host interface transfers are byte-verified here. The
> netem and netns scenarios have working harness logic and run under `sudo` (or
> a privileged CI runner); they are skipped, not failed, without privilege.
> Truly cross-machine scenarios (LAN between two hosts, USB tethering, Ethernet,
> cross-node Tailscale) require a second device and are documented manual steps.

## Manual cross-machine procedure

On the receiver:

```bash
peerbeam receive                 # serves on transfer.port (49600), advertises
```

On the sender (same LAN / tethered / same tailnet):

```bash
peerbeam send bigfile.iso --to "<receiver name>"      # via discovery
peerbeam send bigfile.iso --addr <receiver-ip>:49600  # or direct
```

- **USB tethering:** tether the phone, confirm both hosts share the tether
  subnet (`ip addr`), then use `--addr <peer-ip>:49600`.
- **Different subnets / internet:** use Tailscale (`--addr <peer-tailscale-ip>`)
  — discovery won't cross subnets, but the tailnet address is directly dialable.
- **Latency/loss on a real link:** apply `tc qdisc … netem` on the sender's
  egress interface and repeat the transfer.

## Notes

- QUIC keep-alive (5 s) + a 30 s idle timeout keep paused/slow transfers alive
  and make transfers robust under latency and loss.
- IPv6 and hostname/MagicDNS targets are handled by the transport's address
  resolver (correct `[::1]:port` bracketing, `to_socket_addrs` for names).
- See [Benchmarks](BENCHMARKS.md) for throughput methodology and
  [Networking](NETWORKING.md) for the transport design.
