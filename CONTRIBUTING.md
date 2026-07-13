# Contributing to PeerBeam

Thanks for helping build PeerBeam. This guide covers the setup, the conventions,
and what a mergeable change looks like.

## Ground rules

PeerBeam is built to be maintained for years and to scale to many contributors.
Every change is judged against one question: *would this still be the right
design with a million users and a hundred contributors?* Concretely:

- **Understand before you change.** Read the relevant code and docs; reuse
  existing abstractions instead of duplicating logic.
- **Respect the layering.** Dependencies point inward toward `peerbeam-domain`.
  The domain must not depend on Flutter, tokio, or a concrete adapter. New
  capabilities are new adapters implementing a domain port — see
  [Architecture](docs/ARCHITECTURE.md) and [API](docs/API.md).
- **One responsibility per module.** No God classes, no giant files.
- **No feature without tests and docs.** See [Testing](docs/TESTING.md).

## Prerequisites

- **Rust** ≥ 1.80 (2021 edition) with `rustfmt` and `clippy`.
- **Flutter** (stable) for UI work; Android SDK for the Android target.

## Project layout

```
rust/       Rust workspace (engine, providers, CLI) — the core
flutter/    Flutter app (desktop + Android)
docs/       Documentation
```

## Build & verify

Run the full gate before opening a PR. CI expects all of it to pass.

```bash
# Rust
cd rust
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings   # warnings are errors
cargo fmt --all --check

# Flutter
cd ../flutter
flutter analyze
flutter test
```

The large-file `#[ignore]`d test (>4 GiB) is opt-in:
`cargo test --workspace -- --ignored`.

## Coding standards

**Rust**
- Idiomatic Rust; prefer composition and dependency injection over inheritance.
- Keep functions small and modules cohesive.
- Every public item gets a doc comment. Public APIs are documented, period.
- Avoid unnecessary dependencies.
- Map errors to the existing `DomainError` / `EngineError` variants; don't
  invent parallel error types.

**Flutter**
- Material 3, adaptive/responsive layouts, dark + light.
- Accessibility (Semantics), keyboard support, localization-ready.
- Keep networking/transfer logic out of the UI — call the engine.

## Commits

- Use clear, conventional-style messages: `feat(transfer): …`,
  `fix(cli): …`, `test: …`, `docs: …`, `perf(transfer): …`.
- Explain the *why*, not just the *what*, in the body.
- Small, focused commits. Don't mix unrelated changes.
- Never break backward compatibility silently — document it (and add a
  [Migration](docs/MIGRATION.md) note if user-visible).

## Adding a provider (the common case)

1. Create a crate under `rust/crates/peerbeam-<capability>-<impl>`.
2. Implement the relevant port from `peerbeam-domain::port`.
3. Add unit + integration tests (drive it in isolation).
4. Register it via `EngineBuilder::with_*` where the frontend wires providers.
5. Document it and link it from [Architecture](docs/ARCHITECTURE.md).

No core changes should be needed — if they are, the port may need discussion
first (open an issue).

## Pull requests

A PR is ready when:

- [ ] The full build/test/clippy/fmt gate passes (Rust **and** Flutter).
- [ ] New behaviour has unit + integration tests.
- [ ] Public APIs are documented; affected docs in `docs/` are updated.
- [ ] Commits are focused with meaningful messages.
- [ ] No new technical debt, duplicated logic, or layering violations.

Open an issue first for large or architectural changes so the design can be
agreed before implementation.

## Security

Report vulnerabilities privately rather than in a public issue. See
[Security](docs/SECURITY.md) for the security model and threat scope.

## License

PeerBeam is AGPL-3.0-or-later. By contributing, you agree your contributions are
licensed under the same terms.
