# Changelog

All notable changes to PeerBeam. Format loosely follows
[Keep a Changelog](https://keepachangelog.com/); the project is pre-1.0 and
versioned per [Supported Versions](SUPPORTED_VERSIONS.md).

## [Unreleased]

_Nothing yet._

## [0.2.0] - 2026-07-14 — Beta

First tagged release. See [Release Notes](docs/RELEASE_NOTES_v0.2.0.md).

### Added
- Branding: PeerBeam logo across all platform icons + README banner.
- `LICENSE` (full AGPL-3.0-or-later), Code of Conduct, security policy,
  supported-versions policy, issue/PR templates, dependabot.
- Continuous integration (`.github/workflows/ci.yml`): fmt, clippy
  (`-D warnings`), `cargo test`, examples build, `flutter analyze`/`test`.
- Tag-triggered release workflow that builds artifacts and publishes a GitHub
  Release (Linux + Android without secrets; macOS/Windows when signing secrets
  are set).
- `Cargo.lock` committed for reproducible application builds.
- Docs: Developer Guide, Transfer Protocol, runnable `quic_transfer` example,
  and the M7/M8 readiness + audit reports.

### Fixed
- Security/correctness hardening from a full-project audit: clipboard alloc DoS,
  Windows path-traversal in folder transfer, secure-link nonce-counter desync on
  retry, atomic (unique-temp) writes for trust/checkpoint/config, FFI shutdown
  stopping the daemon, cancel unblocking a pending transfer, poison-tolerant FFI
  locks, device-identity flapping across providers, and Flutter
  notify-after-dispose guards.

### Verified
- 217 Rust tests + 35 Flutter tests pass; clippy/fmt clean; examples compile and
  run byte-exact; Linux release build; live Android→Linux transfer. See
  [Stable Readiness](docs/STABLE_READINESS.md).

## M7 — Documentation, DX & open-source readiness
- README drift fixed; added Developer Guide and Transfer Protocol docs; a
  runnable `quic_transfer` example; CODE_OF_CONDUCT, SECURITY policy,
  SUPPORTED_VERSIONS, issue/PR templates, dependabot; four readiness reports.

## M6 — UI/UX polish
- Friendly, actionable error text (no internal detail leaks); screen-reader
  announcements on transfer cards; UX docs.

## M5 — Validation & hardening
- Full quality gate clean; folder edge-case tests; security review (no critical
  issues); benchmarks; Beta-readiness report; live Android→Linux transfer.

## M1–M4
- Rust engine, QUIC transport, RouteManager, discovery, FFI (M1–M3), Dart SDK +
  repositories, live-only Flutter, packaging. See [Migration](docs/MIGRATION.md).
