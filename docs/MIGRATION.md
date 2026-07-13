# Migration Guide

PeerBeam **v2** is a ground-up rewrite. This guide explains what changed from
v1, what has feature parity today, what is still in progress, and how to move
across.

## Why v2

v1 proved the idea; v2 makes it maintainable at scale. The rewrite exists to:

- Enforce **Clean Architecture** — a reusable Rust core with a domain that
  depends on nothing, and swappable adapters behind ports. See
  [Architecture](ARCHITECTURE.md).
- Make PeerBeam **plugin-friendly** — discovery, transport, storage, crypto,
  trust are all interchangeable adapters, not baked-in code.
- Share **one core across frontends** — the Flutter app and the CLI are thin
  clients over the same engine, and a future Web/API frontend can be too.
- Raise the bar on **security, testing, and streaming** as first-class concerns
  rather than add-ons.

## What changed structurally

| Concern | v1 | v2 |
|---|---|---|
| Core | Monolithic | Rust workspace of focused crates; domain is the dependency sink. |
| Extensibility | Hard-coded mechanisms | Ports (traits) + adapter crates registered via `EngineBuilder`. |
| Discovery | Single/limited | LAN UDP + mDNS + Tailscale, merged and deduplicated into one list. |
| Transfer | — | Streaming, chunked, resumable, whole-file SHA-256, folder structure, never loads a whole file into RAM. |
| Security | — | X25519 + AES-256-GCM, mutual auth, TOFU trust pinning, per-frame replay protection, atomic safe writes. |
| Frontends | GUI | Flutter app **and** a first-class CLI over the same engine. |
| Config | v1 format | Typed `EngineConfig`, JSON on disk (see below). |

## Feature status in v2

**Implemented and tested**
- Multi-provider discovery + merge/dedup ([Discovery](DISCOVERY.md), [Devices](DEVICES.md)).
- Transfer engine: single file, folders, clipboard, resume, integrity, retry,
  pause/cancel ([Transfer](TRANSFER.md), [Clipboard](CLIPBOARD.md)).
- Security layer: auth, `SecureLink`, trust, safe writes ([Security](SECURITY.md)).
- **QUIC transport + networked transfer.** `send`/`receive`/`daemon` work end
  to end over QUIC with mutual authentication — via discovery (`send --to`) or a
  direct `--addr`. Verified by a two-process integration test ([CLI](CLI.md),
  [Networking](NETWORKING.md)).
- CLI: `discover`, `list`, `status`, `config`, `doctor`, `benchmark`,
  `completions`, `send`, `receive`, `daemon start` ([CLI](CLI.md)).
- Flutter UI (desktop + Android), drag & drop, notifications ([UI](UI.md), [Android](ANDROID.md)).

**In progress / gated**
- **Folder send** over the network is not wired yet (single files only); the
  folder engine exists but the CLI path sends files. `clipboard`/`history` CLI
  execution and `daemon stop|status` IPC remain gated (exit code 8).
- **Automatic route switching** mid-transfer (resume is already implemented;
  switching lands next).
- **Folder receive** does not yet use the `.part`/atomic-finalize path that
  single-file receive uses.
- **Stable device identity.** The CLI uses an ephemeral per-run keypair/id, so
  TOFU re-pins each run; persisting a long-term identity is a follow-up.

Track these in the relevant component docs; the [README](../README.md) status
box is the quick reference.

## Configuration migration

v2 stores a typed `EngineConfig` as JSON. There is no automatic importer from a
v1 config — recreate your settings with the CLI:

```bash
peerbeam config path                                  # where v2 keeps it
peerbeam config set device.name "My Laptop"
peerbeam config set storage.save_directory "/downloads"
peerbeam config set transfer.chunk_size 1048576
peerbeam config show                                  # verify
```

Notes:
- Missing required fields are a hard error (no silent half-load); unknown extra
  fields are ignored (forward-compatible).
- Defaults are sensible — you only need to set what you want to change. Deleting
  the file falls back to defaults.

## Trust / paired devices

v2 uses trust-on-first-use: a peer's fingerprint is pinned on first contact and
a later fingerprint change is rejected. There is no import of v1 pairings —
devices are re-pinned on first connection in v2. For stronger assurance, compare
fingerprints out of band. See [Security](SECURITY.md).

## For integrators

If you embedded or scripted v1:
- Replace direct calls with the v2 engine API — build an `Engine` via
  `EngineBuilder`, subscribe to its event/device streams, and run transfers with
  the `peerbeam-transfer` functions. See [API](API.md).
- Scripts should target the CLI's `--json` output and stable exit codes rather
  than parsing human text. See [CLI](CLI.md).

## Roadmap after transport

Once the QUIC transport lands: end-to-end network `send`/`receive`, the daemon
and its IPC, automatic route selection/switching, real-network benchmarks, and
the deferred folder-finalize hardening. Longer term: additional discovery
providers (Bluetooth, ZeroTier, relay) and iOS/Web frontends.
