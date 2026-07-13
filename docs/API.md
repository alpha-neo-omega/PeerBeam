# API

The PeerBeam engine is a reusable Rust library. Frontends (the Flutter app, the
CLI, anything you build) construct an `Engine` from providers and drive it.
This document is the embedding guide. For the layering behind it see
[Architecture](ARCHITECTURE.md).

> Versioning: the workspace is `0.2.x` and pre-1.0 — the API may change between
> minor versions. Crates are path/workspace members today; they are not yet
> published to crates.io.

## The building blocks

- **Ports** (`peerbeam-domain::port`) — traits your adapters implement.
- **`EngineBuilder`** (`peerbeam-engine`) — registers providers, validates, and
  builds an `Engine`.
- **`Engine`** — the runtime handle: config, provider registry, event streams,
  discovery control, device list.
- **Transfer functions** (`peerbeam-transfer`) — free async functions that run a
  transfer over any `Link`.

## Building an engine

```rust
use std::sync::Arc;
use peerbeam_engine::{EngineBuilder, EngineConfig};

// Adapters (each implements a domain port):
use peerbeam_discovery_udp::UdpDiscovery;
use peerbeam_storage_fs::FsStorage;
use peerbeam_crypto::AeadCrypto;
use peerbeam_trust_fs::FsTrust;
use peerbeam_reliability_fs::FsReliability;

let engine = EngineBuilder::new(EngineConfig::default())
    .with_discovery(Arc::new(UdpDiscovery::new(my_device_id)))
    .with_storage(Arc::new(FsStorage::new()))
    .with_encryption(Arc::new(AeadCrypto::new()))
    .with_trust(Arc::new(FsTrust::new("/var/lib/peerbeam/trust")))
    .with_reliability(Arc::new(FsReliability::new("/var/lib/peerbeam/checkpoints")))
    .build()?;
```

`EngineBuilder` methods (all take `Arc<dyn Port>`, all chainable):
`with_discovery`, `with_transfer`, `with_route`, `with_encryption`,
`with_compression`, `with_reliability`, `with_storage`, `with_trust`,
`with_notification`, `with_clipboard`. Use `EngineBuilder::with_defaults()` to
start from `EngineConfig::default()`. `build()` returns `Result<Engine,
EngineError>`.

## Driving the engine

```rust
// Configuration and providers
let cfg: &EngineConfig = engine.config();
let registry = engine.registry();          // access registered providers

// Events (broadcast; subscribe before starting work)
let mut events = engine.subscribe();        // broadcast::Receiver<DomainEvent>
let mut devices = engine.device_changes();  // broadcast::Receiver<DeviceChange>

// Discovery
engine.start_discovery(me_device).await?;   // `me` = this device's identity
let snapshot = engine.devices();            // Vec<ManagedDevice>, merged/deduped
engine.record_device_latency(&id, Some(12));
engine.stop_discovery().await?;

// Consume events
while let Ok(ev) = events.recv().await {
    // DomainEvent::PeerDiscovered / PeerUpdated / PeerLost / TransferProgress / …
}
```

`Engine` methods:

| Method | Returns | Purpose |
|---|---|---|
| `config()` | `&EngineConfig` | Effective configuration. |
| `registry()` | `&ProviderRegistry` | Registered providers. |
| `subscribe()` | `broadcast::Receiver<DomainEvent>` | All domain events. |
| `publish(event)` | `usize` | Emit an event (n receivers). |
| `start_discovery(me)` / `stop_discovery()` | `Result<()>` | Run/stop all discovery providers. |
| `devices()` | `Vec<ManagedDevice>` | Current merged device list. |
| `device_changes()` | `broadcast::Receiver<DeviceChange>` | Device add/update/remove stream. |
| `record_device_latency(id, ms)` | `()` | Feed latency into route selection. |

## Running a transfer

Transfers are free functions over any `Link` — you supply the link (from a
`TransferProvider`, or an in-process one for tests). Nothing loads a whole file
into memory.

```rust
use peerbeam_transfer::{send_file, receive_file, SendRequest, TransferControl};
use tokio::sync::mpsc;

let storage = FsStorage::new();
let ctrl = TransferControl::new();          // pause / resume / cancel
let (progress_tx, mut progress_rx) = mpsc::unbounded_channel();

let req = SendRequest {
    transfer_id: "t1".into(),
    name: "movie.mkv".into(),
    path: "/data/movie.mkv".into(),
    size: file_len,
    chunk_size: 1024 * 1024,
};

// `link` is any `&mut dyn Link`.
let outcome = send_file(&mut link, &storage, req, &ctrl, &progress_tx, /*retries*/ 3).await?;
// On the other end:
let received = receive_file(&mut link, &storage, "/downloads", &ctrl, &progress_tx).await?;
```

`peerbeam-transfer` also exports:

| Item | Purpose |
|---|---|
| `send_file` / `receive_file`, `SendRequest`, `Received`, `TransferOutcome` | Single-file streaming with resume + SHA-256. |
| `send_folder` / `receive_folder`, `FolderSendRequest`, `FolderReceived` | Recursive folder transfer, structure preserved, resumable. |
| `send_clipboard` / `receive_clipboard` | Text/URL/image/code clipboard payloads. |
| `send_file_recover` / `receive_file_recover`, `LinkFactory` | Auto-reconnect + resume driver for interrupted transfers. |
| `TransferControl` | Pause / resume / cancel a running transfer. |
| `authenticate`, `Identity`, `Session`, `SecureLink` | Mutual auth + secured framing (see [Security](SECURITY.md)). |
| `Control`, `TransferMeta` | Wire protocol types. |

Progress arrives as `Progress` values on the channel: transferred/total bytes,
current file, files completed/total, status. See [Transfer](TRANSFER.md) for the
protocol and [Clipboard](CLIPBOARD.md) for clipboard specifics.

## Implementing a provider

To add a capability, implement the relevant port and register it. Example
skeleton for a transport:

```rust
// Signature types (Route, TransferSession, Bind) live in peerbeam-domain.
use async_trait::async_trait;
use futures::stream::BoxStream;
use peerbeam_domain::port::{TransferProvider, Link};

struct QuicTransport { /* … */ }

#[async_trait]
impl TransferProvider for QuicTransport {
    // Outbound: open a link to a peer over the chosen route.
    async fn dial(&self, route: &Route, session: &TransferSession) -> Result<Box<dyn Link>> { /* … */ }
    // Inbound: accept links on a bind address.
    async fn serve(&self, bind: Bind) -> Result<BoxStream<'static, Result<Box<dyn Link>>>> { /* … */ }
}

let engine = EngineBuilder::with_defaults()
    .with_transfer(Arc::new(QuicTransport::new()))
    .build()?;
```

The engine and every layer above it work against the trait, so no core code
changes when you swap or add an adapter. This is how discovery, storage, crypto,
trust, and (soon) the network transport are all wired.

## Errors

Domain operations return `peerbeam_domain::error::Result<T>` with
`DomainError` (`Storage`, `Transfer`, `Connection`, `Integrity`, `Cancelled`,
…). Engine construction returns `EngineError`. Map these to your frontend's
error surface — the CLI maps them to stable exit codes (see [CLI](CLI.md)).

## Frontend bridge

This document covers embedding the engine in Rust. The Flutter app instead uses
the stable C-ABI bridge — see [FFI](FFI.md).
