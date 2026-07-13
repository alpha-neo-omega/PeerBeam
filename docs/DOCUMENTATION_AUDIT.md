# Documentation Audit — M7

Audit of the documentation set against the implementation. Goal: every doc
matches the code, links resolve, and a new contributor can build/understand/
contribute without external help. No product features were changed.

Legend: ✓ Verified · 🟡 Code-reviewed · ⚪ Environment-limited.

## Method

- Enumerated all `*.md` (excluding `flutter/build`, `rust/target`).
- Scanned every relative link across README, root docs, `docs/`, `.github/`,
  and `examples/` — resolved to files on disk.
- Cross-checked high-drift-risk claims (status, quick-start commands, wire
  protocol, crate list, gated features) against source.

## Findings & actions

| # | Finding | Severity | Action |
|---|---|---|---|
| 1 | README status box was stale (pre-transport wording) | High | ✓ Fixed — now 🟢 Beta with live-verified Android→Linux note |
| 2 | README quick-start "Try it" claimed transfer was gated | High | ✓ Fixed — real `receive`/`send` example over QUIC |
| 3 | No `TRANSFER_PROTOCOL.md` (wire format undocumented) | Medium | ✓ Added, drawn from `peerbeam-transfer` source |
| 4 | No `DEVELOPER_GUIDE.md` (onboarding scattered) | Medium | ✓ Added — layout, build/run/test, "where to change what" |
| 5 | No runnable example | Medium | ✓ Added `quic_transfer` example (compiles + runs) + `examples/README.md` |
| 6 | Missing OSS/GitHub files (CoC, SECURITY policy, templates) | High | ✓ Added — see [Open Source Readiness](OPEN_SOURCE_READINESS.md) |
| 7 | README docs index didn't list new docs | Low | ✓ Fixed — index links DEVELOPER_GUIDE + TRANSFER_PROTOCOL |
| 8 | Broken relative links | — | ✓ None found across the set |

## Coverage matrix

| Area | Doc | State |
|---|---|---|
| Overview / quick start | README | ✓ Aligned |
| Contributing | CONTRIBUTING | ✓ Aligned |
| Onboarding | DEVELOPER_GUIDE | ✓ New |
| Architecture / ports | ARCHITECTURE | 🟡 Reviewed, aligned |
| Networking / discovery | NETWORKING, DISCOVERY | 🟡 Reviewed, aligned |
| Transfer engine | TRANSFER | 🟡 Reviewed, aligned |
| Wire protocol | TRANSFER_PROTOCOL | ✓ New, source-accurate |
| Security | SECURITY, SECURITY_REPORT | 🟡 Reviewed, aligned |
| FFI / API | FFI, API | 🟡 Reviewed — see [API Review](API_REVIEW.md) |
| CLI | CLI | 🟡 Reviewed; gated commands noted (clipboard/history, daemon stop\|status) |
| Build / install / release | BUILD, INSTALL, RELEASE | 🟡 Reviewed; Win/macOS ⚪ config-complete, host build pending |
| Readiness / known gaps | BETA_READINESS, KNOWN_ISSUES | 🟡 Reviewed, aligned |

## Known documentation limitations

- Windows/macOS build/packaging docs are ⚪ config-complete but not host-built
  in this environment — flagged as such in BUILD/INSTALL/RELEASE and
  [Beta Readiness](BETA_READINESS.md).
- Wire format has no version byte yet (governed by release version) — tracked
  in [Known Issues](KNOWN_ISSUES.md) for 1.0.

## Verdict

Documentation is consistent with the implementation and complete for Beta.
Remaining items are host-build verification (environment-limited), not doc gaps.
