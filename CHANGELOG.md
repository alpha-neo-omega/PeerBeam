# Changelog

All notable changes to PeerBeam. Format loosely follows
[Keep a Changelog](https://keepachangelog.com/); the project is pre-1.0 and
versioned per [Supported Versions](SUPPORTED_VERSIONS.md).

## [Unreleased]

Release-candidate hardening (M8) — independent release audit; no product
features added.

### Added
- `LICENSE` — full AGPL-3.0-or-later text at the repository root.
- Continuous integration (`.github/workflows/ci.yml`): fmt, clippy
  (`-D warnings`), `cargo test`, examples build, `flutter analyze`/`test` on
  every push and PR.
- `Cargo.lock` is now committed for reproducible application builds.
- Release-audit reports: `docs/FINAL_AUDIT.md`, `STABLE_READINESS.md`,
  `FINAL_SECURITY_REVIEW.md`, `FINAL_PERFORMANCE_REVIEW.md`,
  `FINAL_COMPATIBILITY_MATRIX.md`, `RELEASE_NOTES_v1.0.md`,
  `LONG_TERM_ROADMAP.md`.

### Verified
- 204 Rust tests + 35 Flutter tests pass; clippy/fmt clean; examples compile
  and run byte-exact; release binaries build (Linux). See
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
