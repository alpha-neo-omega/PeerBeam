# PeerBeam v0.2.1 — Beta

Patch release over [v0.2.0](RELEASE_NOTES_v0.2.0.md). Same Beta status; the
headline change is that the **`peerbeam` CLI now ships for every desktop OS**,
plus the security/correctness and branding fixes made since 0.2.0.

## What's new

- **CLI in the release** — standalone `peerbeam` binary attached for Linux,
  macOS (arm64), and Windows, with Linux shell completions. First-class for
  headless servers, SSH, and scripting. See [CLI](CLI.md) for install.
- **Branding** — PeerBeam logo across all platform icons + README banner.
- **Hardening** (full-project audit): clipboard alloc DoS, Windows folder-path
  traversal, secure-link nonce-counter desync on retry, atomic (unique-temp)
  writes for trust/checkpoint/config, FFI shutdown stops the daemon, cancel
  unblocks a pending transfer, poison-tolerant FFI locks, device-identity
  flapping, and Flutter notify-after-dispose guards.

## Verified

- 217 Rust tests + 35 Flutter tests pass; clippy (`-D warnings`) and fmt clean.
- v0.2.0 verified live end-to-end on real hardware (Android → Linux over QUIC,
  byte-exact, mutual auth + TOFU, owner-only file perms).

## Artifacts

- **CLI (all desktops)** — `peerbeam-linux-x64` (+ `peerbeam.bash` / `_peerbeam`
  / `peerbeam.fish`), `peerbeam-macos-arm64`, `peerbeam-windows-x64.exe`.
  Unsigned — macOS/Windows may warn on a browser download; see [CLI](CLI.md).
- **Desktop app (Linux)** — `peerbeam-*-linux-x64.tar.gz` + `.deb`.
- **Android** — APK/AAB (debug-signed unless keystore secrets are set).
- **Windows / macOS desktop app (DMG/MSIX)** — not attached until signing
  secrets exist; build from source or sign locally per [RELEASE](RELEASE.md).

## Install

CLI: download the binary for your OS, make it executable, put it on `PATH` —
[CLI § Install](CLI.md). Linux app: extract the tarball or install the `.deb`.
See [Install](INSTALL.md).

## Known limitations

Windows/macOS desktop-app signing not yet automated; macOS CLI is arm64 only;
no persistent device identity; CLI `clipboard`/`history` and `daemon
stop|status` partial. See [Known Issues](KNOWN_ISSUES.md) and the
[Roadmap](LONG_TERM_ROADMAP.md).

## License

AGPL-3.0-or-later.
