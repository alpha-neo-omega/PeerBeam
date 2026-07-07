# PeerBeam v2

Zero-configuration, secure, cross-platform file & clipboard sharing.

This tree currently contains the **v2 architectural foundation** only — the
clean-architecture skeleton the rest of the product is built on. No transfer,
discovery, or crypto behaviour is implemented yet; those arrive as providers
plugged into the seams defined here.

## Rust workspace (`rust/`)

Dependencies point inward. `domain` is the dependency sink.

| Crate | Layer | Responsibility |
|-------|-------|----------------|
| `peerbeam-domain` | Domain | Entities, **ports (interfaces)**, events, errors. Zero IO/runtime. |
| `peerbeam-platform` | Platform | OS detection, host identity, standard directories. |
| `peerbeam-config` | Config | Typed, layered `EngineConfig` with load/save. |
| `peerbeam-telemetry` | Logging | Structured `tracing` setup for frontends. |
| `peerbeam-app` | Application | **Dependency-injection registry**; use-case seams. Depends only on domain ports. |
| `peerbeam-engine` | Composition root | `EngineBuilder` wires providers → `Engine` handle + event stream. |

```
domain  ◄── platform ◄── config ◄── telemetry
   ▲            ▲           ▲
   └── app ◄────┴── engine ─┘   (engine = only multi-dependency crate)
```

### Build & verify

```bash
cd rust
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Ports (interfaces)

Discovery · Transfer (+ `Link`/`Frame`) · Route · Encryption · Compression ·
Reliability · Storage · Trust · Notification · Clipboard. Each is a trait in
`peerbeam-domain::port`; adapters implement them and are registered via the
engine builder.

## License

AGPL-3.0-or-later.
