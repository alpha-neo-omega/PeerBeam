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

### Transport status

The **QUIC transport is implemented** (`peerbeam-transfer-quic`, built on
[quinn](https://docs.rs/quinn)) — the first production `TransferProvider`.

- **`dial(route, session)`** opens an outbound connection over the chosen route
  and returns a `Link`. **`serve(bind)`** binds a UDP/QUIC endpoint and yields
  inbound `Link`s as peers connect.
- Each `Link` is one QUIC **bidirectional stream** with length-delimited
  framing; the transfer engine runs over it unchanged.
- **Zero-config TLS.** QUIC mandates TLS, but there is no PKI: each node uses a
  fresh self-signed certificate and the client accepts any server cert. Real
  peer identity comes from the application-layer `SecureLink` handshake, not
  from certificates — QUIC alone is encrypted but unauthenticated by design (see
  [Security](SECURITY.md)).
- **Disconnects** surface as `DomainError::Connection`; the engine's recovery
  driver (`send_file_recover`/`LinkFactory`) can redial and resume from the
  receiver's offset.
- Verified by **two-real-endpoint** integration tests (localhost, not mocks)
  and measured by `peerbeam benchmark quic` — see [Benchmarks](BENCHMARKS.md).

Still in progress: wiring QUIC into the end-user CLI `send`/`receive`/`daemon`
commands (peer→route resolution + a receiving daemon loop) — the provider and
engine are ready; those commands remain gated (exit code 8) until wired. See
[Migration](MIGRATION.md).

The in-process `Link` (bounded channels) remains for tests and
`benchmark loopback` as a transport-free upper bound.

## Security on the wire

Discovery input is untrusted by design; authentication happens at transfer time,
not in discovery. Once a `Link` is open, `authenticate` performs a mutual
X25519 handshake with HMAC key confirmation and TOFU trust pinning, then
`SecureLink` seals every frame with AES-256-GCM and rejects replays. Full detail
in [Security](SECURITY.md).
