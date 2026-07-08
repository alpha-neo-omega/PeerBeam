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
  change.
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
