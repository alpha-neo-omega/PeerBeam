# Developer Guide

Everything you need to go from a fresh clone to a merged change. Start with
[Contributing](../CONTRIBUTING.md) for the ground rules; this guide is the
hands-on companion — how the workspace is laid out, how to build/run/test each
piece, and where to make a given kind of change.

## Prerequisites

| Tool | Version | For |
|---|---|---|
| Rust | ≥ 1.80 (2021 edition) | engine, CLI, FFI |
| `rustfmt`, `clippy` | bundled with Rust | formatting, lint gate |
| Flutter | stable | the app |
| Android SDK/NDK | current | Android target |

See [Install](INSTALL.md) and [Build](BUILD.md) for platform-specific setup and
packaging.

## Repository layout

```
rust/       Rust workspace — the core (engine, providers, CLI, FFI)
flutter/    Flutter app — desktop (Win/macOS/Linux) + Android
docs/       Component and top-level documentation
```

### Rust workspace (Clean Architecture)

Dependencies point **inward** toward `peerbeam-domain`, which defines the
*ports* (traits) every provider implements. Nothing in `domain` depends on
Flutter, tokio, or a concrete adapter.

| Crate | Layer | Responsibility |
|---|---|---|
| `peerbeam-domain` | domain | Entities, ids, errors, **ports** (traits) |
| `peerbeam-app` | application | Use-cases wiring ports together |
| `peerbeam-engine` | composition | Builds the app from concrete adapters |
| `peerbeam-config` | infra | Settings load/store |
| `peerbeam-telemetry` | infra | Structured logging/tracing |
| `peerbeam-platform` | infra | OS/platform facts |
| `peerbeam-discovery-udp` / `-mdns` / `-tailscale` | adapter | Discovery providers |
| `peerbeam-transfer` | adapter | Transfer engine (auth, secure framing, stream, folder, recover) |
| `peerbeam-transfer-quic` | adapter | QUIC transport (`Link`) |
| `peerbeam-storage-fs` | adapter | Filesystem storage |
| `peerbeam-crypto` | adapter | X25519 / AES-256-GCM |
| `peerbeam-trust-fs` | adapter | TOFU trust store |
| `peerbeam-reliability-fs` | adapter | Resume checkpoints |
| `peerbeam-clipboard-mem` | adapter | In-memory clipboard |
| `peerbeam-ffi` | boundary | C-ABI cdylib for Flutter |
| `bins/peerbeam-cli` | frontend | The `peerbeam` CLI |

See [Architecture](ARCHITECTURE.md) for data flow and the port catalogue, and
[API](API.md) for embedding the engine.

### Flutter app

```
flutter/lib/sdk/    Dart SDK over the FFI (PeerBeamApi + PeerBeam impl, models, events, exceptions)
flutter/lib/data/   Repositories (event-driven ChangeNotifiers, no polling)
flutter/lib/state/  AppState / AppScope
flutter/lib/features/  Screens
```

The SDK is the only thing that talks to the engine; UI talks to repositories.
See [FFI](FFI.md) for the bridge and [UI](UI.md) for the app.

## Build & run

### CLI

```bash
cd rust
cargo build --release -p peerbeam-cli
./target/release/peerbeam --help
./target/release/peerbeam doctor        # environment check
```

Full command reference: [CLI](CLI.md).

### The app

```bash
cd flutter
flutter run        # desktop, or an attached Android device
```

The app loads the native FFI library; build it first with
`rust/../scripts/build-ffi.sh` (see [Build](BUILD.md)) if running against a
fresh checkout.

### Run the end-to-end example

A complete file transfer over the real QUIC transport in one process —
handshake → SecureLink → stream:

```bash
cd rust
cargo run --example quic_transfer -p peerbeam-cli
```

Source: `rust/bins/peerbeam-cli/examples/quic_transfer.rs`. Good first stop for
understanding the transfer API end to end.

## Test & lint (the merge gate)

Run all of this green before opening a PR — it mirrors CI and the
[PR checklist](../.github/PULL_REQUEST_TEMPLATE.md):

```bash
cd rust
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --examples                  # examples must compile

cd ../flutter
flutter analyze
flutter test
```

See [Testing](TESTING.md) for the test strategy and
[Network Testing](NETWORK_TESTING.md) for real-network integration tests.

## Where do I make this change?

| I want to… | Go to |
|---|---|
| Add a discovery method | New `peerbeam-discovery-*` crate implementing the discovery port |
| Add a transport (e.g. WebRTC) | New adapter implementing `Link`; register in the engine |
| Change the wire format | `peerbeam-transfer` (`protocol.rs`/`folder.rs`) + [Transfer Protocol](TRANSFER_PROTOCOL.md) |
| Add an FFI call | `peerbeam-ffi` + Dart binding in `flutter/lib/sdk/ffi` + [FFI](FFI.md) |
| Add a CLI command | `bins/peerbeam-cli` + [CLI](CLI.md) |
| Change a screen | `flutter/lib/features/…`; data via a repository, never direct FFI |
| Change crypto/trust | `peerbeam-crypto` / `peerbeam-trust-fs` + [Security](SECURITY.md) |

Rule of thumb: a new capability is a new **adapter implementing a domain
port**, not a change to the domain. If you find yourself widening a port, stop
and check the design against [Architecture](ARCHITECTURE.md).

## Conventions

- **Idiomatic Rust / Flutter.** Small functions, cohesive modules, no God
  classes, no giant files. Public items get doc comments.
- **Respect layering.** Domain depends on nothing outward. Adapters depend on
  domain ports, never on each other.
- **No feature without tests and docs.** Update the relevant doc in the same
  change; the docs index lives in the [README](../README.md).
- **Commits** are conventional (`feat(scope):`, `fix(scope):`, …) with a clear
  body; see recent history for the style.

## Troubleshooting the dev loop

Common build/run problems (missing FFI lib, toolchain versions, device
discovery) are collected in [Troubleshooting](TROUBLESHOOTING.md).
