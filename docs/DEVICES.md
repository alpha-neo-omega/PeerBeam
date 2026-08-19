# Device management

The `DeviceManager` is the UI's single source of truth for peers. It sits
between the discovery providers and the frontend, so **no networking or
discovery logic lives in the UI** — the UI queries a snapshot and subscribes
to changes.

```
DiscoveryProviders (udp, mdns, tailscale, …)
        │  per-provider (ProviderId, DiscoveryEvent) streams
        ▼
   DeviceManager (engine)         ← owns tasks + broadcast channel
        │  drives
        ▼
   DeviceStore (app, pure)        ← merge · dedup · online/offline · latency · caps
        │  emits
        ▼
   DeviceChange stream + devices() snapshot  →  UI
```

## Split

| Layer | Type | Role |
|---|---|---|
| domain | `ManagedDevice`, `DeviceCapabilities`, `DeviceChange` | the model + notifications the UI sees |
| app | `DeviceStore` | **pure** reducer — all logic, no IO, unit-tested |
| engine | `DeviceManager` | async: advertise+scan providers, merge streams, broadcast changes |

Keeping the logic in a pure store means every rule is tested deterministically
without sockets or timers; the manager only owns runtime concerns.

## Responsibilities

- **Merge providers** — combines every registered `DiscoveryProvider`'s
  event stream (`select_all`) into one fold.
- **Remove duplicates** — one `ManagedDevice` per `DeviceId` no matter how
  many providers report it; addresses are unioned across providers.
- **Online / offline** — a device stays tracked when its last provider drops
  it, flipped `online = false` (UI greys it out) rather than deleted;
  `DeviceStore::prune` removes long-gone devices.
- **Latency** — `record_device_latency` stores a per-device RTT fed by the
  networking layer (measurement is not the manager's job) and notifies on
  change. What feeds it is `RouteManager::record_link_rtt`, wired to the engine
  with `RouteManager::reporting_to`: the FFI reads
  `QuicChannels::rtt()` — quinn's own smoothed estimate for the connection the
  session is already running over — once the PeerSession handshake completes,
  and reports it under the id that session's row is keyed by (the discovery id
  on a dial, the authenticated peer id on an accept). There is **no PeerBeam
  ping**: no probe, no timer, no message type, nothing added to the wire.

  `QuicChannels::rtt()` returns `Option` because quinn's `rtt()` does not: before
  its first ACK-driven sample it hands back the *configured initial* RTT
  (333 ms, RFC 9002 §6.2.2), which is a constant and not a measurement. `None`
  means "no sample yet" and clears the figure rather than leaving an older one
  standing.

  **Direct-vs-relay is not reported, because nothing in this codebase can
  observe it.** QUIC has no notion of a relay — a relay is indistinguishable
  from the peer at the socket — and quinn's `ConnectionStats`/`PathStats` carry
  no such field. `RouteKind::Relay` exists but no production path constructs one
  (`RouteManager::with_relays` has no caller outside its own tests), and
  Tailscale's DERP relaying happens below our socket: we dial a `100.x`/`fd7a:`
  address either way. The only source would be `tailscale status --json`'s
  `CurAddr`/`Relay` fields, which `peerbeam-discovery-tailscale` does not parse
  and which describe Tailscale's path rather than the route PeerBeam picked.
- **Capabilities** — derived from the capabilities of the providers seeing a
  device: `reachable_lan`, `reachable_remote`, `requires_tailscale`, and the
  provider set. Lets the UI badge devices and route selection prefer local
  paths.
- **Notify UI** — `DeviceChange` events (`Added` / `Updated` /
  `StatusChanged` / `LatencyChanged` / `Removed`) plus a `devices()`
  snapshot. A plain re-sighting that changes nothing emits **no** event, so
  the UI never churns.

## Engine API (what the UI calls)

```
engine.start_discovery(me)          // advertise + scan all providers
engine.devices() -> Vec<ManagedDevice>   // current snapshot, online first
engine.device_changes() -> Receiver<DeviceChange>   // subscribe
engine.record_device_latency(id, ms)
engine.stop_discovery()
```

## Testing

- **Unit** (`DeviceStore`): add/dedup, capability derivation (LAN vs
  remote vs Tailscale-only), silent re-sighting, partial vs final provider
  loss (online→offline), rediscovery, latency-on-change, prune, snapshot
  ordering — all pure and deterministic.
- **Integration** (`peerbeam-engine/tests`): two providers with different
  capabilities registered via the builder, driven through
  `start_discovery`, asserting dedup, merged addresses, capability flags,
  offline transition, and latency — via the engine's public API.
  `route_manager.rs::a_measured_round_trip_reaches_the_device_list_and_is_announced`
  walks the whole path: a measurement recorded on a wired `RouteManager` lands
  on the row *and* is announced as `LatencyChanged`, rounds 2.7 ms to 3 rather
  than flooring it, and clears on `None`.
- **Transport** (`peerbeam-transfer-quic/tests/channels.rs`):
  `a_live_connection_reports_a_measured_round_trip_time` holds two real QUIC
  endpoints open and asserts both sides report a loopback round trip far below
  the 333 ms initial constant — the one assertion that would still fail if the
  "has an ACK arrived yet" guard were dropped.
