# Contributor Experience — M7

A walk through the path a new contributor takes, from landing on the repo to a
mergeable PR, noting where the road is smooth and where friction remains. The
goal of M7 was that this path needs **no external guidance**.

Legend: ✓ Smooth · 🟡 Works, minor friction · ⚪ Environment-limited.

## The journey

### 1. Land on the repo → understand what it is (✓)
README opens with a one-line pitch, an honest 🟢 Beta status box (what's
verified live vs pending), highlights, repo layout, and a full docs index.
A newcomer knows in under a minute what PeerBeam is and what state it's in.

### 2. Decide whether to trust it (✓)
License, [Code of Conduct](../CODE_OF_CONDUCT.md), [Security policy](../SECURITY.md),
and a privacy stance (no accounts/telemetry/cloud) are all one click away.
Security internals are documented, not hand-waved.

### 3. Get it building (✓ Linux/Android · ⚪ Win/macOS)
[Developer Guide](DEVELOPER_GUIDE.md) gives prerequisites, layout, and exact
build/run commands for both the CLI and the app; [Build](BUILD.md) /
[Install](INSTALL.md) cover packaging. Linux + Android are verified; Windows/
macOS are config-complete but need a host to build — clearly flagged so a
contributor isn't surprised.

### 4. Understand the code (✓)
Clean Architecture is explained in [Architecture](ARCHITECTURE.md); the
Developer Guide's crate table and a "where do I change what?" map point a
contributor straight to the right module. The runnable `quic_transfer` example
demonstrates the core transfer API end to end in one file.

### 5. Make a change the right way (✓)
The Developer Guide states the layering rule (new capability = new adapter
implementing a domain port), conventions (small modules, docs on public items,
conventional commits), and the "no feature without tests and docs" rule.

### 6. Validate before pushing (✓)
The exact merge gate is written down and mirrors CI:
`fmt` / `clippy -D warnings` / `test` / `build --examples` / `flutter analyze` /
`flutter test`. No guessing what CI will check.

### 7. Open the PR (✓)
Issue templates (bug/feature) and a PR template with an explicit checklist
(layering, tests, docs, no debt) make expectations concrete. Dependabot keeps
dependencies current so contributors aren't fighting stale lockfiles.

## Friction points found & resolved in M7

| Friction | Resolution |
|---|---|
| Stale README status implied transfer didn't work | ✓ Updated to live-verified Beta |
| No single onboarding doc (info scattered) | ✓ Added Developer Guide |
| No runnable example to learn the API from | ✓ Added `quic_transfer` (compiles + runs) |
| Example snippets used wrong API names | ✓ Corrected to real symbols |
| Missing CoC / security policy / issue+PR templates | ✓ All added |
| Wire format only in source comments | ✓ Added [Transfer Protocol](TRANSFER_PROTOCOL.md) |

## Remaining friction (honest)

- **Win/macOS contributors** can't verify packaging without a host (⚪); the
  docs say so up front.
- **First-run FFI library** must be built before `flutter run` against a fresh
  checkout — documented in Developer Guide / [Troubleshooting](TROUBLESHOOTING.md),
  but not yet a one-command bootstrap. 🟡

## Verdict

A motivated contributor on Linux/Android can go clone → understand → build →
change → validate → PR using only the in-repo docs. Windows/macOS packaging is
the one path still gated on environment, and it is clearly labelled.
