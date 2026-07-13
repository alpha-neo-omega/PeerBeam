# Changelog

## Unreleased — M6 UI/UX polish
- Errors: engine failures now shown as friendly, actionable text; no internal
  (`quic`/FFI/exception) detail leaks to users.
- Accessibility: transfer cards announce a full summary to screen readers.
- Verified: `flutter analyze` clean, `flutter test` green, no regressions.
- Docs: UX_AUDIT, DESIGN_DECISIONS, KNOWN_UI_LIMITATIONS, UX_NOTES.

## M5 — Validation & hardening
- Full quality gate clean; folder edge-case tests; security review (no critical
  issues); benchmarks; Beta-readiness report. Live Android→Linux transfer.

## M1–M4
- Rust engine, QUIC transport, RouteManager, discovery, FFI (M1–M3), Dart SDK +
  repositories, live-only Flutter, packaging. See docs/MIGRATION.md.
