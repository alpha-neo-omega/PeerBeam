# Architecture

PeerBeam is built as a reusable core with interchangeable adapters and thin
frontends. The guiding rule: **dependencies point inward**, toward a domain that
knows nothing about Flutter, tokio, sockets, or the filesystem.

## Layers

```
        ┌─────────────────────────────────────────────┐
        │  Frontends:  Flutter app   ·   peerbeam CLI  │
        └───────────────────────┬─────────────────────┘
                                 │ uses
        ┌───────────────────────▼─────────────────────┐
        │  Composition root:  peerbeam-engine          │  wires providers,
        │  (EngineBuilder → Engine)                     │  owns event streams
        └───────────────────────┬─────────────────────┘
                                 │ depends on
        ┌───────────────────────▼─────────────────────┐
        │  Application:  peerbeam-app                   │  DI registry,
        │  (ProviderRegistry, DeviceStore, merge)       │  use-case seams
        └───────────────────────┬─────────────────────┘
                                 │ depends only on
        ┌───────────────────────▼─────────────────────┐
        │  Domain:  peerbeam-domain                     │  entities, PORTS,
        │  (entities · ports/traits · events · errors)  │  events, errors
        └───────────────────────▲─────────────────────┘
                                 │ implement ports
        ┌───────────────────────┴─────────────────────┐
        │  Adapters (plugins):                          │
        │  discovery-udp / -mdns / -tailscale ·         │
        │  transfer · storage-fs · crypto ·             │
        │  reliability-fs · trust-fs · clipboard-mem    │
        └───────────────────────────────────────────────┘
```

The **domain** is the dependency sink. Everything else depends on it; it depends
on nothing internal. Adapters depend on the domain only to *implement its
ports*. The engine is the only crate that knows about many things at once —
that's its job as the composition root.

## Crates

| Crate | Layer | Responsibility |
|---|---|---|
| `peerbeam-domain` | Domain | Entities, **ports (traits)**, events, errors, id newtypes. Zero IO/runtime. |
| `peerbeam-platform` | Platform | OS/host detection, standard directories, hostname. |
| `peerbeam-config` | Config | Typed `EngineConfig` with JSON load/save and defaults. |
| `peerbeam-telemetry` | Logging | `tracing` subscriber setup for frontends. |
| `peerbeam-logs` | Logging | Bounded in-memory log ring (500 lines) plus an optional file, so a one-shot command can read logs a previous process wrote. |
| `peerbeam-app` | Application | `ProviderRegistry` (DI), `DeviceStore` reducer, cross-provider `merge_discovery`. |
| `peerbeam-engine` | Composition root | `EngineBuilder` wires providers → `Engine` handle + event/device streams + `DeviceManager` + `RouteManager` (route selection/failover/migration). |
| `peerbeam-discovery-udp` | Adapter | LAN UDP broadcast discovery. |
| `peerbeam-discovery-mdns` | Adapter | mDNS/DNS-SD discovery. |
| `peerbeam-discovery-tailscale` | Adapter | Tailscale discovery via CLI + LocalAPI. |
| `peerbeam-transfer` | Adapter/logic | Streaming file/folder/clipboard transfer, resume, auth, `SecureLink`. |
| `peerbeam-transfer-quic` | Adapter | QUIC `TransferProvider` (quinn) — real network `Link`s via `dial`/`serve`. |
| `peerbeam-chunk` | Logic | Content-defined chunking (Gear rolling hash; 16/64/256 KiB min/avg/max) so an edited file re-sends only the chunks that changed. |
| `peerbeam-chat` | Capability | Chat over PeerSession: text and files, history in an encrypted `AppStore` namespace, search, per-conversation retention. |
| `peerbeam-clipboard` | Capability | Clipboard sync over PeerSession. Both directions are gated on approval **and** the Clipboard permission; a refused inbound clip is dropped and logged rather than failing the channel. Local clipboard history. |
| `peerbeam-presence` | Capability | Device-status heartbeat (battery, storage, network, version) — opt-in *and* trusted-only. |
| `peerbeam-notes` | Capability | Notes kept on this device and exchanged with peers granted the `notes` permission; a delete leaves a tombstone. |
| `peerbeam-browse` | Capability | Read-only listing of folders a device chose to share. The share list is empty by default. |
| `peerbeam-sync` | Capability | Bidirectional folder reconciliation on per-file version vectors; conflicts are kept, not resolved. Bytes ride the Transfer channel. |
| `peerbeam-pairing` | Capability | Optional six-digit PIN pairing: an HMAC over the handshake transcript, so a first contact cannot be silently intercepted. |
| `peerbeam-ffi` | Boundary | Stable C-ABI bridge to Flutter: discovery, transfers, clipboard, settings, daemon, status, logs + events. See [FFI](FFI.md). |
| `peerbeam-storage-fs` | Adapter | Streaming filesystem `StorageProvider` + atomic finalize. |
| `peerbeam-crypto` | Adapter | X25519 + AES-256-GCM `EncryptionProvider`. |
| `peerbeam-reliability-fs` | Adapter | Checkpoint store for resume. |
| `peerbeam-trust-fs` | Adapter | TOFU trust pinning (`TrustStore`). |
| `peerbeam-identity-fs` | Adapter | `IdentityStore`: the device's long-term keypair in one atomically written `0600` JSON file. |
| `peerbeam-appstore-fs` | Adapter | `AppStore`: per-namespace keyed records, each value encrypted at rest with AES-256-GCM. |
| `peerbeam-spaces` | Adapter | Named local sets of trusted devices over `AppStore` + `TrustStore`. Nothing goes on the wire — a fan-out is N ordinary 1:1 sends. |
| `peerbeam-wake` | Adapter | Wake-on-LAN: the magic packet, the remembered MAC, and the approval gate. Local broadcast domain only. |
| `peerbeam-clipboard-mem` | Adapter | In-memory `ClipboardProvider`. |
| `peerbeam-update` | Utility | One HTTPS GET, only when a person asks, answering whether a newer release exists (amendment A1). Nothing downloads. |
| `bins/peerbeam-cli` | Frontend | `peerbeam` command-line tool. |

## Ports (the seams)

Every capability is a trait in `peerbeam-domain::port`. Adapters implement them;
the engine builder registers them. This is what makes PeerBeam plugin-friendly —
a new discovery mechanism or transport is a new crate implementing one trait, no
core changes.

| Port | Purpose |
|---|---|
| `DiscoveryProvider` | Find peers; stream device changes. |
| `TransferProvider` + `Link`/`Frame` | Open a connection; send/recv framed bytes. |
| `ChannelTransport` | Open/accept independent streams on one connection, each a `Link`. |
| `RouteProvider` | Choose the best route to a peer. |
| `EncryptionProvider` | Keypair, ECDH, seal/open, fingerprint. |
| `CompressionProvider` | Optional payload compression. |
| `ReliabilityStore` | Persist/restore transfer checkpoints. |
| `StorageProvider` | Streamed read/write, size, list, atomic finalize. |
| `AppStore` | Namespaced keyed records for capability data, encrypted at rest. |
| `TrustStore` | Pin/lookup/trust device fingerprints. |
| `IdentityStore` | Load/persist this device's long-term keypair and id. |
| `NotificationSink` | Surface events to the user. |
| `ClipboardProvider` | Read/write the system clipboard. |

## Data flow

**Discovery → device list.** Each `DiscoveryProvider` emits `DeviceChange`s.
`peerbeam-app`'s `merge_discovery` + `DeviceStore` deduplicate across providers
(a device seen on LAN *and* Tailscale is one entry) and track online/offline.
The engine's `DeviceManager` exposes a merged `Vec<ManagedDevice>` and a
`device_changes()` broadcast stream the UI subscribes to. See
[Devices](DEVICES.md) and [Discovery](DISCOVERY.md).

**Transfer.** A `TransferProvider` yields a `Link` (ordered framed byte pipe).
The transfer engine (`send_file`/`receive_file`, `send_folder`, clipboard)
streams chunks over it with progress, pause, cancel, retry, resume, and
whole-file SHA-256. For secured transfers the `Link` is first wrapped by
`authenticate` → `SecureLink` (per-frame AEAD + replay protection). See
[Transfer](TRANSFER.md) and [Security](SECURITY.md).

## Design rules

- The domain never imports Flutter, tokio, or a concrete adapter.
- Communicate across layers through ports, not concrete types.
- One responsibility per module; no God classes, no giant files.
- Adapters are swappable and independently testable (the test suite drives the
  transfer engine over an in-memory `Link`, no network needed —
  see [Testing](TESTING.md)).

## Frontends

Both the Flutter app and the CLI are thin: they build an `Engine` via
`EngineBuilder`, subscribe to its streams, and render. No networking or transfer
logic lives in a frontend. The Rust core is reusable by GUI, CLI, and any future
Web/API frontend — see [API](API.md).
