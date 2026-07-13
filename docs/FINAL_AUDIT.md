# Final Repository Audit — M8

Independent release-engineering audit of the whole repository, conducted from
scratch against the **current** implementation. Prior milestone claims were not
trusted; every statement below is backed by an inspection or a command run in
this session.

Legend: ✓ Verified (ran/observed here) · 🟡 Code-reviewed · ⚪ Environment-limited.

## Snapshot

| Metric | Value | How |
|---|---|---|
| Rust source | ~16,886 LOC across 17 crates + 1 bin | `wc -l` |
| Flutter source | ~3,877 LOC (`lib/`) | `wc -l` |
| Rust tests | **204 passed, 0 failed** | `cargo test --workspace` ✓ |
| Flutter tests | **35 passed** (10 files) | `flutter test` ✓ |
| Clippy | **0 warnings** (`-D warnings`, all targets) | ✓ |
| Format | clean | `cargo fmt --all --check` ✓ |
| Examples | compile + run byte-exact | `cargo build --examples`, `cargo run --example quic_transfer` ✓ |
| Release build | CLI 8.1 MB, FFI 7.1 MB | `cargo build --release` ✓ |
| `TODO`/`FIXME`/`XXX` | **0** | `grep` |
| Version | **0.2.0** (VERSION, workspace, pubspec) | ✓ |
| Git tags | **none** | `git tag` |

## Architecture

Clean Architecture, dependencies pointing inward to `peerbeam-domain` (ports as
traits); adapters implement ports; `peerbeam-engine` is the composition root.
Crate sizes are healthy — the two largest are `peerbeam-ffi` (2027) and
`peerbeam-transfer` (1977); no God file. 🟡 Layering verified by inspection and
the crate graph; matches [Architecture](ARCHITECTURE.md). No architectural flaw
found.

## Folder structure, naming, workspace

`rust/` (crates + bins), `flutter/`, `docs/`, `examples/`, `packaging/`,
`scripts/`, `.github/`. Consistent `peerbeam-*` crate naming; FFI symbols
`pb_*`. Workspace organization is idiomatic. ✓

## Dependencies

33 direct workspace dependencies; mainstream, well-maintained crates (tokio,
quinn, rustls, serde, x25519-dalek, aes-gcm, sha2, hmac). `cargo generate-lockfile`
noted newer majors available for `socket2` (0.5→0.6) and `x25519-dalek`
(2.0→3.0) — not upgraded (out of scope; no security driver identified without a
scanner). Vulnerability scan is ⚪ (see [Final Security Review](FINAL_SECURITY_REVIEW.md)).

## Code quality / dead code / duplication

- 0 `TODO`/`FIXME`. No `unimplemented!`/`todo!` in shipping paths observed.
- **213 `unwrap`/`expect` in non-test src.** The 62 in `peerbeam-ffi` are almost
  all `Mutex::lock().unwrap()` (poison) and serialize-can't-fail expects; the
  FFI boundary is **panic-safe** — every `extern "C"` entry runs inside a
  `catch_unwind` `guard()` (verified in `lib.rs`), so an internal panic returns
  an error envelope rather than unwinding into the caller. 🟡 Non-FFI unwraps
  (storage/trust/discovery) are mostly on invariants; not audited line-by-line.
- No duplicate business logic found; frontends (CLI, Flutter) share the one
  engine. ✓

## Public APIs

Rust ports, C-ABI FFI, and the Dart SDK are internally consistent — see
[API Review](API_REVIEW.md). No missing-doc output on sampled `peerbeam-domain`
public items.

## Configuration / build / release scripts

- `scripts/`: `build-ffi.sh`, `package-{linux,windows.ps1,macos,android}.sh`,
  `set-version.sh`. Single-source `VERSION` file.
- Android `build.gradle.kts` uses real release keys when `key.properties` is
  present, else debug keys (documented fallback). `key.properties` absent
  (secret, not committed) — store-signed APK is ⚪.

## CI

**Gap found & fixed:** the repo had only `release.yml` (tag-triggered) — no
test/lint gate on push/PR. Added `.github/workflows/ci.yml` running the full
gate on every push and PR. `release.yml` builds Linux/Windows/macOS on `v*` tag
(Windows/macOS jobs ⚪ not host-run here).

## Documentation

36 docs pre-M8 + this milestone's reports. README, CONTRIBUTING, DEVELOPER_GUIDE
present. Link scan across all `*.md` resolves (✓). Details in
[Documentation Audit](DOCUMENTATION_AUDIT.md); doc verification for M8 in
[Stable Readiness](STABLE_READINESS.md).

## Security / performance / UX

Summarized in [Final Security Review](FINAL_SECURITY_REVIEW.md),
[Final Performance Review](FINAL_PERFORMANCE_REVIEW.md), and the UX docs. No
critical issue found; the transport uses an accept-any TLS verifier **by
design**, with authentication done at the application layer (X25519 mutual auth
+ per-frame AES-256-GCM + TOFU).

## Release-blocking findings (fixed in M8 unless noted)

| # | Finding | Severity | Status |
|---|---|---|---|
| 1 | No `LICENSE` file despite AGPL-3.0 declared | **Blocker** | ✓ Fixed — full AGPL text added |
| 2 | `Cargo.lock` gitignored (app reproducibility) | **Blocker** | ✓ Fixed — now committed |
| 3 | No CI test/lint workflow | High | ✓ Fixed — `ci.yml` added |
| 4 | CHANGELOG stale (“Unreleased — M6”) | Medium | ✓ Fixed |
| 5 | Version is 0.2.0, not 1.0.0; no tags | **Blocker for “Stable”** | ⚠ Not bumped — decision is RC, not Stable (see below) |
| 6 | Cross-platform / transport matrix largely unverified | **Blocker for “Stable”** | ⚪ Environment-limited |
| 7 | No README badges/screenshots | Low | Open (cosmetic) |
| 8 | **App shipped the default Flutter logo** on Android/macOS/Windows (only Linux packaging used the custom bolt) — missed in the initial audit pass | High | ✓ Fixed — `flutter_launcher_icons` wired to a single brand master (`packaging/icon-1024.png`, rendered from `peerbeam.svg`); all platform launcher icons regenerated and visually verified |

## Verdict

Engineering quality is high and the automated gate is fully green. The blockers
to a **Stable v1.0 label** are release hygiene (now largely fixed) plus
cross-platform verification that this environment cannot provide. See
[Stable Readiness](STABLE_READINESS.md) for the gate decision.
