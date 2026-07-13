# PeerBeam v1.0 — Release Notes (DRAFT)

> **Status: DRAFT — not yet released.** The M8 release gate concluded
> [🟢 Release Candidate](STABLE_READINESS.md), not Stable. These notes are
> prepared so the 1.0 tag is a formality once the remaining
> [blockers](STABLE_READINESS.md#blockers-between-rc-and-stable-v10) (Windows/
> macOS host builds, real transport matrix, dependency scan, version bump + tag)
> are cleared. Do not publish until those are ✓.

PeerBeam is a zero-configuration, secure, cross-platform file & clipboard
sharing app — a modern take on LAN file sharing. Open the app, see nearby
devices, click, send. No IP addresses, no pairing codes, no accounts, no cloud.

## Highlights

- **Zero config** — automatic discovery merged across LAN broadcast, mDNS, and
  Tailscale into one device list.
- **Works where LAN doesn't** — Tailscale support (CLI + LocalAPI, MagicDNS).
- **Streaming everything** — unlimited file size, chunked, never loads a whole
  file into RAM; folders keep their structure.
- **Resumable & verified** — receiver-reported offsets, whole-file SHA-256,
  automatic retry, atomic safe writes.
- **Secure by default** — X25519 mutual authentication, per-frame AES-256-GCM
  with replay protection, TOFU trust pinning; received files are owner-only
  (`0600`).
- **Two frontends, one core** — Material 3 Flutter app + scriptable,
  SSH-friendly CLI over the same Rust engine.
- **Private** — no accounts, no telemetry, no cloud dependency.

## What's verified at release-gate time

- ✓ 204 Rust tests + 35 Flutter tests pass; clippy/fmt clean; examples run
  byte-exact.
- ✓ Linux release build (CLI + FFI); loopback throughput **726 MiB/s**.
- ✓ One live cross-device transfer (Android → Linux over LAN, byte-exact).

See [Stable Readiness](STABLE_READINESS.md) and
[Final Compatibility Matrix](FINAL_COMPATIBILITY_MATRIX.md) for the full,
honest support status.

## Install

- **Linux**: `dist/peerbeam-<version>-linux-x64.tar.gz`, or build from source
  (`cargo build --release -p peerbeam-cli`). See [Install](INSTALL.md).
- **Android**: signed APK (requires `key.properties` for a store build).
- **Windows / macOS**: packaging is configured; host-built artifacts pending
  (see blockers).

Full instructions: [Install](INSTALL.md) · [Build](BUILD.md) · [Release](RELEASE.md).

## Upgrade

Pre-1.0 releases may contain breaking changes ([Supported Versions](../SUPPORTED_VERSIONS.md)).
The FFI ABI is versioned independently (`pb_abi_version = 1`); a Flutter app and
native library must share the same ABI version. No persisted on-disk transfer
state format change is required to move from 0.2.x to 1.0.

## Rollback

The app keeps no server-side or cloud state; rolling back is reinstalling the
previous package. Trust pins and settings live in the platform app directory and
are backward-compatible within the 1.0 line. Removing the app removes its local
state.

## Known limitations (carried into 1.0)

- Windows/macOS/broad-transport support is code-complete but not host-verified
  in the audit environment.
- No persistent device identity yet (peers re-pin after restart).
- CLI `clipboard`/`history` and `daemon stop|status` are partial.
- Desktop OS notifications / tray not yet implemented.

Tracked in [Known Issues](KNOWN_ISSUES.md) and the
[Long-Term Roadmap](LONG_TERM_ROADMAP.md).

## Thanks

Built as an open-source project under AGPL-3.0-or-later. Contributions welcome —
see [Contributing](../CONTRIBUTING.md) and the [Developer Guide](DEVELOPER_GUIDE.md).
