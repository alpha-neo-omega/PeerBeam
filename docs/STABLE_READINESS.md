# Stable Readiness — M8 Release Gate

The release-gate decision for PeerBeam, produced by an independent audit against
the current implementation. Optimized for accuracy, not for a positive outcome.

Legend: ✓ Verified here · 🟡 Code-reviewed · ⚪ Environment-limited.

## Decision

# 🟢 Release Candidate

**Stable v1.0 is not justified yet.** The engineering is RC-grade — the full
automated gate is green, security is sound, the release build works, and one
real cross-device transfer is verified. But a *Stable v1.0* claim requires two
things this audit cannot in good faith assert: (1) the version/release provenance
of an actual 1.0 (the tree is `0.2.0`, untagged), and (2) verification of the
cross-platform and cross-transport support v1.0 would advertise — which this
single-Linux-host environment cannot provide. Shipping "Stable" now would
inflate quality against unverified platforms.

## Gate scorecard

| Dimension | Grade | Evidence |
|---|---|---|
| Code quality | ✓ Strong | 0 TODO/FIXME; clippy `-D warnings` clean; fmt clean; no God files |
| Architecture | 🟡 Strong | Clean Architecture, inward deps, ports as traits; no flaw found |
| Security | 🟡 Strong | app-layer mutual auth, AES-256-GCM + replay counter, 0600 files, path-traversal guard, no secret logging; dep-scan ⚪ |
| Performance | ✓/🟡 Strong | 726 MiB/s loopback; streaming, zero-copy send; scale/startup ⚪ |
| UX | 🟡 Good | Material 3, friendly errors, a11y semantics, keyboard/drag-drop |
| Accessibility | 🟡 Good | screen-reader labels on transfer cards; reduced-motion aware |
| Cross-platform | ⚪ Partial | **Linux ✓, Android+LAN ✓ (prior live); Windows/macOS ⚪** |
| Documentation | ✓ Strong | 36+ docs, links resolve, examples compile; matches implementation |
| Testing | ✓ Strong | 204 Rust + 35 Flutter tests pass; examples run byte-exact |
| Maintainability | ✓ Strong | small cohesive crates; now with CI + committed lockfile |
| Developer experience | ✓ Strong | Developer Guide, runnable example, reproducible gate |
| Release engineering | 🟡 Improved | LICENSE + CI + lockfile + CHANGELOG fixed; version/tag pending |

## Blockers between RC and Stable v1.0

| # | Blocker | Owner action | Env |
|---|---|---|---|
| 1 | Version still `0.2.0`, no git tag | `scripts/set-version.sh 1.0.0`, tag `v1.0.0` after sign-off | can do |
| 2 | Windows host build + smoke transfer | build on a Windows host | ⚪ needs host |
| 3 | macOS host build + smoke transfer | build on a macOS host | ⚪ needs host |
| 4 | Real multi-host transport matrix (Wi-Fi/Ethernet/USB/Tailscale/IPv6/cross-subnet) | run on real networks | ⚪ needs hosts |
| 5 | Dependency vulnerability scan | add `cargo audit`/`cargo deny` to CI, run clean | ⚪ needs network |
| 6 | Store-signed Android release | provide `key.properties` | ⚪ needs secret |

Non-blocking polish: README badges/screenshots; persistent device identity;
CLI `clipboard`/`history` + `daemon stop|status` completeness; desktop
notifications/tray.

## Fixed during this audit (release hygiene)

- ✓ `LICENSE` (full AGPL-3.0-or-later) added at root.
- ✓ `Cargo.lock` committed (reproducible application builds).
- ✓ `ci.yml` added — fmt/clippy/test/examples + flutter analyze/test on push/PR.
- ✓ CHANGELOG updated to a real, current entry.

## Path to Stable v1.0

1. Land blockers #2–#6 (host builds, transport matrix, dep scan, signing) with
   evidence appended to [Final Compatibility Matrix](FINAL_COMPATIBILITY_MATRIX.md).
2. Bump to `1.0.0` (blocker #1), finalize [Release Notes](RELEASE_NOTES_v1.0.md).
3. Tag `v1.0.0`; let `release.yml` build all three desktop targets + Android.
4. Re-run this gate; flip to 🚀 Stable only when cross-platform rows are ✓.

## Bottom line

PeerBeam is a **Release Candidate**: high-quality, well-tested, secure, and now
release-hygienic on Linux. It becomes **Stable v1.0** once the same guarantees
are *verified* on Windows/macOS and across real transports — work that requires
hardware this audit did not have. Accurate status beats an optimistic label.
