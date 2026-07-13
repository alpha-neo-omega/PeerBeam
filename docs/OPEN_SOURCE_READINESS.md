# Open Source Readiness — M7

Checklist of what an open-source project needs to be cloned, understood, built,
trusted, and contributed to by strangers — and where PeerBeam stands.

Legend: ✓ Present · 🟡 Present, follow-up noted · ⚪ Environment-limited.

## Project files

| Item | State | Location |
|---|---|---|
| README with status, quick start, docs index | ✓ | [README](../README.md) |
| License (AGPL-3.0-or-later) | ✓ | declared in README / crate manifests |
| Contributing guide | ✓ | [CONTRIBUTING](../CONTRIBUTING.md) |
| Code of Conduct (Contributor Covenant 2.1) | ✓ | [CODE_OF_CONDUCT](../CODE_OF_CONDUCT.md) |
| Security policy (private disclosure) | ✓ | [SECURITY](../SECURITY.md) |
| Supported versions | ✓ | [SUPPORTED_VERSIONS](../SUPPORTED_VERSIONS.md) |
| Changelog | ✓ | [CHANGELOG](../CHANGELOG.md) |
| Developer guide / onboarding | ✓ | [Developer Guide](DEVELOPER_GUIDE.md) |
| Runnable example | ✓ | `rust/bins/peerbeam-cli/examples/quic_transfer.rs` |

## GitHub collaboration surface

| Item | State | Location |
|---|---|---|
| Issue templates (bug / feature) | ✓ | `.github/ISSUE_TEMPLATE/` |
| Issue chooser config (blank disabled, links) | ✓ | `.github/ISSUE_TEMPLATE/config.yml` |
| Pull request template (with merge checklist) | ✓ | `.github/PULL_REQUEST_TEMPLATE.md` |
| Dependency updates | ✓ | `.github/dependabot.yml` (cargo, pub, actions) |
| Release automation | 🟡 | `.github/workflows/release.yml` — Linux/Android build-verified; Win/macOS jobs ⚪ config-complete, not host-run |

## Quality gate (reproducible by any contributor)

| Gate | State |
|---|---|
| `cargo fmt --all` | ✓ |
| `cargo clippy --workspace --all-targets -- -D warnings` | ✓ |
| `cargo test --workspace` | ✓ |
| `cargo build --examples` | ✓ |
| `flutter analyze` | ✓ |
| `flutter test` | ✓ |

(Gate results recorded in the M7 validation step / executive summary.)

## Trust & transparency

- **Privacy** — no accounts, telemetry, analytics, or cloud dependency
  (stated + reflected in code). ✓
- **Security model** — documented end to end: [Security](SECURITY.md),
  [Security Report](SECURITY_REPORT.md), [Transfer Protocol](TRANSFER_PROTOCOL.md). 🟡
- **Honest status** — README and [Beta Readiness](BETA_READINESS.md) mark what
  is verified live vs code-reviewed vs environment-limited; gated CLI features
  are called out rather than implied complete. ✓

## Remaining gaps (tracked, not blockers for open-sourcing)

| Gap | Where tracked |
|---|---|
| Windows/macOS host build + packaging verification | [Beta Readiness](BETA_READINESS.md), BUILD/RELEASE |
| CLI `clipboard`/`history`, `daemon stop\|status` | [CLI](CLI.md), README |
| Desktop OS notifications / tray | [Known Issues](KNOWN_ISSUES.md) |
| Persistent device identity | [Known Issues](KNOWN_ISSUES.md) |
| Wire-format version negotiation (1.0) | [Known Issues](KNOWN_ISSUES.md) |

## Verdict

PeerBeam meets the bar to be a public open-source repository: license, CoC,
security policy, contributing + developer guides, issue/PR templates, dependency
automation, a reproducible quality gate, and honest status reporting are all in
place. The open gaps are feature/host-verification items, documented and
visible — appropriate for a Beta project.
