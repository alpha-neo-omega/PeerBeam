# PeerBeam v0.2.2 — Beta

The "it just works" release: real progress, persistent everything, and the
share flows people actually use — plus a full visual overhaul.

## Highlights

### Transfers you can trust
- **Receiver-confirmed progress** — the sender's bar tracks what the receiver
  has actually written (dedicated QUIC back-channel), with live speed and ETA.
  No more "100 % but still sending" over slow links.
- **Fail fast, retry smart** — unreachable peers error in ~8 s instead of a
  silent 30 s hang, and transient connect failures retry automatically with
  backoff.
- **Cancel is instant**, even mid-chunk on a slow link.

### It remembers now
- **Settings persist** (device name, save folder, auto-accept, theme, toggles)
  and are applied by the engine at startup.
- **Transfer history survives restarts** (last 500), Clear really clears, and
  **tapping an entry opens the file**.
- **Trusted devices** get a management screen: see every pinned peer's key
  fingerprint, revoke to force re-approval.

### CLI grows up
- `peerbeam clipboard send|get` and `peerbeam history` are real commands now
  (previously gated stubs) — stdin-friendly for headless boxes, NDJSON for
  scripts, same wire convention as the app.

### Sharing, everywhere
- **Android share sheet**: "Share → PeerBeam" from any app now completes the
  whole flow — files or text.
- **Send folders** (desktop picker + drag-and-drop), **send clipboard** with
  one-tap **Copy** on the receiving side.
- **File picking on Android** — the hero action works on every platform.
- **QR**: share a saved device as a QR; scan to add one.
- One **unified destination picker** (Nearby + Saved) — Tailscale/by-address
  peers reachable from every flow.

### Honest presence
- Nearby shows **live peers only** — devices vanish when they go offline, a
  dead peer can't linger via a stale mDNS cache entry, and Scan/Stop reflects
  the real engine state.

### A new look
- Stock Material 3 (baseline palette), **Google Sans Flex** typeface (bundled,
  OFL), flat tonal components, one hero send action, seamless headers,
  sentence-case copy. See `docs/UI_REDESIGN_REPORT.md`.

## Platforms
- Verified live this cycle: Linux (desktop + CLI) and Android (real
  cross-network transfers over Tailscale and LAN).
- CI now proves Windows and macOS (Intel **and** Apple Silicon) on every push:
  full Rust test suite + release desktop builds.

## Gate
- 222 Rust tests, 35 Flutter tests, `clippy -D warnings` clean, release builds
  on all desktop targets + Android.

_Signed macOS/Windows installers still pend signing secrets (see
docs/RELEASE.md); unsigned artifacts build in CI._
