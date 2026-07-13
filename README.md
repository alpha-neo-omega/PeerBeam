<div align="center">

# PeerBeam

**Zero-configuration, secure, cross-platform file & clipboard sharing.**

Open the app → see nearby devices → click → send. No IP addresses, no pairing
codes, no accounts, no cloud.

</div>

---

PeerBeam is a modern take on LAN file sharing. It discovers peers across LAN,
mDNS, and Tailscale at once, merges them into one device list, and streams files
of any size with end-to-end encryption and resumable, integrity-checked
transfers. A Rust engine does the work; a Flutter app and a first-class CLI are
two frontends over the same core.

> **Status.** The engine, discovery, security, transfer pipeline, QUIC
> transport, CLI, and Flutter UI are implemented and tested. Networked
> `send`/`receive` work end to end over QUIC with mutual authentication
> (verified by a two-process integration test). Remaining gaps: folder send,
> `clipboard`/`history` CLI execution, and desktop packaging for Windows/macOS.
> See [Migration](docs/MIGRATION.md) and the per-component docs below.

## Highlights

- **Zero config** — no addresses or codes; discovery is automatic and merged
  across providers.
- **Works where LAN doesn't** — Tailscale support (CLI + LocalAPI, MagicDNS)
  for VPN / headless / cross-network reach.
- **Streaming everything** — unlimited file size, chunked, never loads a whole
  file into RAM; folders keep their structure.
- **Resumable & verified** — receiver-reported offsets, whole-file SHA-256,
  automatic retry, atomic safe writes.
- **Secure by default** — X25519 + AES-256-GCM, mutual authentication, TOFU
  trust pinning, per-frame replay protection.
- **Two frontends, one core** — polished Material 3 Flutter app and a
  scriptable, SSH-friendly CLI.
- **Private** — no accounts, no telemetry, no cloud dependency.

## Repository layout

```
rust/       Rust workspace — engine, providers, CLI (the core)
flutter/    Flutter app — desktop (Win/macOS/Linux) + Android
docs/       Component and top-level documentation
```

The Rust workspace follows Clean Architecture: dependencies point inward toward
`peerbeam-domain`, which defines the *ports* (traits) every provider implements.
See [Architecture](docs/ARCHITECTURE.md).

## Quick start

### Build the CLI

```bash
cd rust
cargo build --release -p peerbeam-cli
./target/release/peerbeam --help
```

### Try it

```bash
peerbeam doctor            # check the environment
peerbeam discover          # find nearby devices
peerbeam list              # show known devices
peerbeam config show       # view configuration
peerbeam benchmark loopback --size 512
```

`send`/`receive` parse and resolve today but stop at a gated message until the
transport lands. Full command reference: [CLI](docs/CLI.md).

### Run the app

```bash
cd flutter
flutter run              # desktop, or an attached Android device
```

## Documentation

| Topic | Doc |
|---|---|
| System design, crates, ports, data flow | [Architecture](docs/ARCHITECTURE.md) |
| Discovery, route selection, link layer | [Networking](docs/NETWORKING.md) · [Discovery](docs/DISCOVERY.md) |
| Transfer engine (stream / folder / resume) | [Transfer](docs/TRANSFER.md) |
| Clipboard sharing | [Clipboard](docs/CLIPBOARD.md) |
| Encryption, auth, trust, safe writes | [Security](docs/SECURITY.md) |
| Embedding the Rust engine | [API](docs/API.md) |
| Command-line interface | [CLI](docs/CLI.md) |
| Flutter UI | [UI](docs/UI.md) |
| Android platform integration | [Android](docs/ANDROID.md) |
| Devices & merge/dedup | [Devices](docs/DEVICES.md) |
| Test strategy | [Testing](docs/TESTING.md) |
| Performance baselines | [Benchmarks](docs/BENCHMARKS.md) |
| Common problems | [Troubleshooting](docs/TROUBLESHOOTING.md) |
| v1 → v2 changes | [Migration](docs/MIGRATION.md) |
| How to contribute | [Contributing](CONTRIBUTING.md) |

## Development

```bash
cd rust
cargo test --workspace              # full test suite
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all

cd ../flutter
flutter test
```

## License

AGPL-3.0-or-later. See [Contributing](CONTRIBUTING.md) for the contribution
flow.
