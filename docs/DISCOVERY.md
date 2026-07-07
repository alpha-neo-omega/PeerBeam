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

### Limits

UDP broadcast does not cross subnets or NAT (`crosses_subnet = false`).
Managed Wi-Fi sometimes filters broadcast — mDNS covers that case.

## mDNS discovery (`peerbeam-discovery-mdns`)

Second provider. Advertises this device as a `_peerbeam._tcp.local.` DNS-SD
service and browses for peers of the same type. Often succeeds on managed
Wi-Fi where UDP broadcast is dropped.

### Design

| Concern | Mechanism |
|---|---|
| Advertise | Register a service; identity in TXT records (`id`, `name`, `device_type`, `platform`, `version`); interface addresses auto-detected (`enable_addr_auto`). |
| Scan | Browse the service type; `ServiceResolved` → `Found`, `ServiceRemoved` → `Lost` (a fullname→id map recovers the device id on removal). |
| Self-filter | Ignore any resolved service whose TXT `id` equals ours. |
| Cross-platform | Delegated to `mdns-sd`; no per-OS code here. |

Identity comes from TXT; addresses come from the resolved records. Also
link-local (`crosses_subnet = false`). `MdnsDiscovery::new` returns an error
if the mDNS daemon can't start, so a host can fall back to UDP-only.

### Testing

- **Unit**: `parse_service` (full record, missing id → `None`, name/type/
  platform defaults) and type/platform mapping — built from an in-memory
  `ServiceInfo`, no network.
- **Integration** (`tests/lifecycle.rs`): advertise → scan → stop runs to
  completion (incl. idempotent repeat calls) without hanging; skips
  gracefully when the daemon is unavailable.

## Merge (`peerbeam_app::merge_discovery`)

Every provider runs independently; the engine fuses their streams into one
deduplicated device list. The pure reducer is `DiscoveryRegistry`.

| Situation | Emitted |
|---|---|
| First sighting of a device (any provider) | `PeerFound` |
| Later sighting that adds info (e.g. a Tailscale address) | `PeerUpdated` (addresses unioned) |
| Redundant sighting, nothing new | *(nothing)* — no UI churn |
| `Lost` from one provider while another still sees it | *(nothing)* |
| `Lost` from the **last** provider seeing it | `PeerLost` |

Each device tracks the set of providers currently reporting it, so a peer
visible via both mDNS and UDP survives either one dropping. The engine's
`start_discovery` advertises + scans every registered provider, runs
`merge_discovery`, and republishes the merged `DomainEvent`s;
`stop_discovery` halts the merge task and every provider.

### Testing the merge

- **Unit** (`DiscoveryRegistry`): found/dedup/union-update/partial-loss/
  final-loss/unknown-loss — pure, deterministic.
- **Integration** (`peerbeam-app/tests/merge.rs`): two fake providers with
  overlapping scripts fused at the stream level.
- **Engine** (`peerbeam-engine/tests/discovery_merge.rs`): two providers
  registered via the builder, merged through `start_discovery`, deduped
  events observed on the engine event stream — end-to-end.

## Next providers

Reaching peers over Tailscale, VPN, or the internet is the job of further
providers implementing the same port (`crosses_subnet = true`), merged
alongside these two with no change to the merge logic.
