# PeerBeam v0.2.0 — Beta

Zero-configuration, secure, cross-platform file & clipboard sharing. Open the
app → see nearby devices → click → send. No IP addresses, no pairing codes, no
accounts, no cloud.

> **Beta.** Engine, discovery, security, QUIC transport, RouteManager, full FFI,
> Dart SDK, and the Flutter app are implemented and tested. Networked
> `send`/`receive` work end to end over QUIC with mutual authentication —
> **verified live on real hardware** (Android → Linux, byte-exact). Stable v1.0
> is gated on Windows/macOS host builds and a real multi-transport matrix — see
> [Stable Readiness](STABLE_READINESS.md).

## Highlights

- **Zero config** — automatic discovery merged across LAN broadcast, mDNS, and
  Tailscale into one device list.
- **Streaming everything** — unlimited file size, chunked, never loads a whole
  file into RAM; folders keep their structure.
- **Resumable & verified** — receiver-reported offsets, whole-file SHA-256,
  automatic retry, atomic safe writes.
- **Secure by default** — X25519 mutual authentication, per-frame AES-256-GCM
  with replay protection, TOFU trust pinning; received files are owner-only.
- **Two frontends, one core** — Material 3 Flutter app + scriptable,
  SSH-friendly CLI over the same Rust engine.
- **Private** — no accounts, no telemetry, no cloud dependency.

## Verified in this release

- 217 Rust tests + 35 Flutter tests pass; clippy (`-D warnings`) and fmt clean.
- Linux release build (CLI + FFI); loopback throughput ~726 MiB/s.
- One live cross-device transfer (Android → Linux over LAN, byte-exact).

Full, honest support status: [Stable Readiness](STABLE_READINESS.md) ·
[Final Compatibility Matrix](FINAL_COMPATIBILITY_MATRIX.md).

## Artifacts

- **Linux** — `peerbeam-*-linux-x64.tar.gz` (attached).
- **Android** — APK (attached; debug-signed unless release keystore secrets are
  configured in CI).
- **Windows / macOS** — packaging is configured but not built in this release
  (host signing secrets not yet set). Build from source meanwhile — see
  [Build](BUILD.md).

## Install

Linux: extract the tarball, or build from source
(`cargo build --release -p peerbeam-cli`). See [Install](INSTALL.md).

## Known limitations

Windows/macOS/broad-transport support is code-complete but not host-verified;
no persistent device identity yet; CLI `clipboard`/`history` and
`daemon stop|status` are partial; no desktop OS notifications/tray. Tracked in
[Known Issues](KNOWN_ISSUES.md) and the [Long-Term Roadmap](LONG_TERM_ROADMAP.md).

## License

AGPL-3.0-or-later. Contributions welcome — see
[Contributing](../CONTRIBUTING.md) and the [Developer Guide](DEVELOPER_GUIDE.md).
