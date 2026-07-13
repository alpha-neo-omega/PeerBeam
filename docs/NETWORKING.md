# Networking

PeerBeam's networking goal is **zero configuration**: the user never types an IP
address, port, or pairing code. This document covers the three networking
concerns — discovery, route selection, and the link layer — and how they fit
together. For discovery internals see [Discovery](DISCOVERY.md); for the
transfer protocol on top of a link see [Transfer](TRANSFER.md).

## 1. Discovery — finding peers

Multiple discovery providers run at once and their results are merged into one
device list. The user never learns which provider found a device.

| Provider | Crate | Reaches |
|---|---|---|
| LAN UDP broadcast | `peerbeam-discovery-udp` | Same broadcast domain (LAN, Ethernet, Wi-Fi, USB tethering). |
| mDNS / DNS-SD | `peerbeam-discovery-mdns` | Same multicast domain; standard, plays well with other tools. |
| Tailscale | `peerbeam-discovery-tailscale` | Tailnet peers over VPN / across networks / headless, via `tailscale status --json` and the LocalAPI, with MagicDNS names. |

Each implements the `DiscoveryProvider` port and emits `DeviceChange` events.
`peerbeam-app::merge_discovery` + `DeviceStore` deduplicate across providers by
device identity — a peer seen on both LAN and Tailscale is a single entry with
multiple reachable addresses — and track online/offline. See
[Devices](DEVICES.md).

Planned providers (ports already exist, adapters do not): Bluetooth, ZeroTier,
and an internet relay.

## 2. Route selection — choosing how to connect

A device may be reachable several ways at once (a LAN address *and* a Tailscale
IP). The `RouteProvider` port selects the best one. The intended priority,
fastest first:

```
LAN  →  USB tethering  →  Ethernet  →  Wi-Fi  →  Tailscale direct
     →  direct internet  →  relay
```

The engine records per-device latency (`record_device_latency`) to inform the
choice. The design calls for automatic route switching, reconnect, and resume
when a route changes mid-transfer — resume is already implemented at the
transfer layer (see below); automatic switching lands with the transport.

## 3. Link layer — moving bytes

Discovery and route selection decide *who* and *how*; the link layer moves the
bytes. It is defined by two domain ports:

- **`TransferProvider`** — opens a connection to a peer and returns a `Link`.
- **`Link`** — an ordered, framed, bidirectional byte pipe: `send_frame`,
  `recv_frame`, `close`. A `Frame` is a `kind` (Meta / Chunk / Control) plus a
  raw `Bytes` payload.

Everything above the link is transport-agnostic. The transfer engine, the
authentication handshake, and `SecureLink` all operate on any `Link`, so a
future QUIC or TCP transport plugs in without touching transfer logic.

```
 TransferProvider ──connect──▶  Link  (ordered framed bytes)
                                  │
                    authenticate  │  (mutual X25519 + key confirmation)
                                  ▼
                              SecureLink   (per-frame AES-256-GCM + replay guard)
                                  │
                                  ▼
             send_file / receive_file / send_folder / clipboard
```

Because a `Link` preserves order, data chunks carry no index — the receiver
appends them in arrival order, keeping per-chunk overhead to zero.

### Current transport status

No production network transport ships yet. The planned transport is **QUIC over
HTTP/3** (multiplexed, congestion-controlled, encrypted). Until it lands:

- The transfer pipeline is fully implemented and exercised over an **in-process
  `Link`** (bounded channels) in tests and `peerbeam benchmark loopback` — this
  gives real backpressure and validates streaming/resume/cancel without a
  network.
- CLI `send`/`receive`/`daemon` parse and resolve but stop at a gated message
  (exit code 8).

See [Benchmarks](BENCHMARKS.md) for measured throughput and
[Migration](MIGRATION.md) for the roadmap.

## Security on the wire

Discovery input is untrusted by design; authentication happens at transfer time,
not in discovery. Once a `Link` is open, `authenticate` performs a mutual
X25519 handshake with HMAC key confirmation and TOFU trust pinning, then
`SecureLink` seals every frame with AES-256-GCM and rejects replays. Full detail
in [Security](SECURITY.md).
