# Changelog

All notable changes to PeerBeam. Format loosely follows
[Keep a Changelog](https://keepachangelog.com/); the project is pre-1.0 and
versioned per [Supported Versions](SUPPORTED_VERSIONS.md).

## [Unreleased]

### Changed
- In-app logo is a monochrome brand glyph tinted to the app's primary colour at
  runtime — matches the theme and stays visible in both light (deep purple) and
  dark (light purple); the earlier transparent mark's white paper-plane washed
  out on light surfaces. Window title reads "PeerBeam" (proper case) on Linux,
  macOS, and Windows.
- Removed the duplicate "PeerBeam" heading on Home — the nav rail (or, on
  phones, the app bar) carries the brand once. Dropped the example
  name/IP placeholders in the add-device and send-to-address dialogs.

## [0.2.2] - 2026-07-15 — Beta

See [Release Notes](docs/RELEASE_NOTES_v0.2.2.md).

### Added
- **CLI folder transfer**: `peerbeam send <dir>` streams whole folders, and
  `receive` dispatches folder transfers (previously file-only on both sides).
- **CLI clipboard + history**: `peerbeam clipboard send` (argument, stdin, or
  system clipboard; same wire convention as the app, so receivers offer Copy),
  `clipboard get` (prints the newest received text), and `peerbeam history`
  (persisted, `--limit`/`--clear`, human or NDJSON). Both were gated stubs.
- **Settings persist** and reach the engine: device name, save directory,
  auto-accept, theme, and toggles survive restarts and apply at init.
- **Transfer history persists** across restarts (bounded to the most recent
  500); Clear now clears engine-side too.
- **Auto-retry**: transient connect failures retry twice with backoff before
  failing.
- **Trusted devices**: Settings lists every pinned device with its key
  fingerprint; revoke to require fresh approval on the next connection.
- **Edit saved devices**: rename or re-address a saved device from its menu
  (share QR / edit / remove).
- **Open the save folder** from Settings (desktop).
- **Open from History**: history entries record the item's local path; tap to
  open the file (or the save folder for folder receives) with the OS handler.
- **Clipboard receive**: a received clipboard payload shows a snackbar with the
  sender, a preview, and one-tap **Copy** — clipboard to clipboard.
- **Android share sheet**: sharing files/text to PeerBeam now completes the
  flow — files open the staged sheet, text offers one-tap send.
- **Send folders**: desktop folder picker + folder drag-and-drop; staged
  batches split into file and folder transfers automatically.
- **Receiver-confirmed progress**: the sender's bar tracks the receiver's real
  byte count over a dedicated QUIC back-channel (falls back to bytes-sent for
  old/non-QUIC peers); 64 KiB chunks + throttled emission for smooth movement,
  a 1s heartbeat so speed/ETA keep ticking, and speed/ETA shown on transfer
  cards.
- **Android file picking**: the Send files action uses the native picker on
  every platform (no more desktop-only gate).
- **Unified destination picker**: one sheet with Nearby + Saved sections for
  file and clipboard sends — saved (Tailscale/by-address) peers are now
  reachable from the phone flows.
- **Send clipboard**: sends the OS text clipboard to a chosen device as a
  `.txt`.
- **QR**: share a saved device as a `peerbeam://` QR; scan one to add it
  (mobile camera).
- **Device search** on Home.

### Changed
- **UI overhaul**: stock Material 3 look (baseline seed), Google Sans Flex
  typeface (bundled, OFL), flat tonal components, one hero send action,
  sentence-case terse copy, seamless app bars, responsive polish. See
  `docs/UI_REDESIGN_REPORT.md`.
- **Nearby devices show live peers only** — offline devices disappear instead
  of lingering greyed out; the Scan/Stop control reflects the engine state
  from boot.

### Fixed
- Tailscale-discovered peers are now reachable: they were stamped with port 0
  ('not reachable right now' on send) because `tailscale status` reports only
  tailnet IPs. Both frontends now stamp the configured transfer port on
  Tailscale peers. Live-verified desktop -> phone over Tailscale.
- Windows GUI no longer flashes a console window on every discovery tick
  (the Tailscale status probe now spawns with CREATE_NO_WINDOW).
- Folder transfers no longer silently drop zero-byte files (both the send-side
  resume skip and the receiver's completed-count treated `0 >= 0` as "already
  transferred").
- A config file from an older or newer version now loads (missing fields fall
  back to defaults) instead of failing to parse; corrupt values still error.
- Cancelling a transfer takes effect immediately, even mid-chunk on a slow
  link.
- Dialing an unreachable peer fails in ~8s with a clear error instead of a
  silent 30s hang.
- A dead peer no longer stays "online" via a stale mDNS cache claim after an
  unclean exit; stopping discovery marks all devices offline instead of
  freezing stale presence.
- Fast transfers always emit a final progress update (fixed a flaky FFI test).

## [0.2.1] - 2026-07-14 — Beta

See [Release Notes](docs/RELEASE_NOTES_v0.2.1.md).

### Added
- The standalone `peerbeam` **CLI now ships in releases** for Linux, macOS
  (arm64), and Windows, with Linux shell completions. Dedicated `cli-*` CI jobs
  build them; the release attaches them alongside the Linux app + Android.
- Local signing how-tos for macOS (Developer ID + notarytool) and Windows
  (MSIX) in [RELEASE](docs/RELEASE.md); `msix_config.publisher` field.

### Note
- Signed macOS/Windows desktop apps (DMG/MSIX) are still not attached to
  releases until host signing secrets are configured.

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
