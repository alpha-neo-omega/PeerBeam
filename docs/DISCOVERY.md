# Discovery

Discovery answers one question with zero configuration: *which devices are
reachable right now?* Each mechanism is a plugin implementing the
`DiscoveryProvider` port (`peerbeam-domain::port::discovery`). The engine
runs many at once and merges their events into one deduplicated device list;
frontends never learn which mechanism found a device.

## LAN discovery (`peerbeam-discovery-udp`)

The first provider. Finds peers on the same broadcast domain — Wi-Fi,
Ethernet, USB tethering — over UDP broadcast.

### Design

One reuse-enabled UDP socket, bound to a well-known port (default `49500`),
handles both send and receive:

| Concern | Mechanism |
|---|---|
| Advertise | Periodic broadcast of a JSON `Announce` (identity + transfer port). |
| Scan | On start, broadcast a `Query` so existing peers announce *immediately* — fast discovery, no waiting for the next interval. Then listen. |
| Automatic refresh | Re-announce every `interval`; a reaper expires peers unheard for `peer_ttl` and emits `Lost`. Self-healing, no manual refresh. |
| Cross-platform | `SO_REUSEADDR` everywhere, `SO_REUSEPORT` on Unix, broadcast enabled. No per-OS code in the discovery logic. |

Defaults: `interval = 2s`, `peer_ttl = 6s` (survives two missed announces).

### Wire protocol (v1)

A single small JSON datagram, versioned so builds never misinterpret each
other (wrong version → ignored):

```json
{ "v": 1, "kind": "announce", "id": "...", "name": "...",
  "device_type": "Phone", "platform": "android", "port": 4200 }
```

`kind` is `announce` or `query`. A peer's **address is taken from the UDP
source**, never the self-reported field, so an advertisement cannot redirect
a connection. Discovery only *finds* devices — authentication and trust
happen later in the transfer handshake.

### Event semantics

- `Found` — first sighting of a peer.
- `Updated` — a known peer's identity fields changed (a pure liveness
  refresh emits nothing, so the UI isn't spammed every 2s).
- `Lost` — peer aged out past `peer_ttl`, or `stop` was called.

The provider self-filters its own broadcast echo by device id.

### Testing

- **Unit** (`proto.rs`, `peers.rs`): wire encode/decode + version/garbage
  rejection, source-IP handling, identity-change detection, and the
  Found/Updated/silent/expire state machine — all pure, no IO.
- **Integration** (`tests/loopback.rs`): a real socket over loopback driven
  by a plain `UdpSocket` peer — discovery, self-filter, query-response, and
  TTL expiry, end-to-end, without needing a broadcast-capable network.

### Limits / next providers

UDP broadcast does not cross subnets or NAT (`crosses_subnet = false`).
Reaching peers over Tailscale, VPN, or the internet is the job of other
providers that implement the same port and are merged alongside this one.
