# PeerBeam — Feature Roadmap

Future work, ranked by value × fit-with-mission. Grounded in the current
codebase (QUIC transport, LAN + mDNS + Tailscale discovery, TOFU trust,
file/folder/clipboard transfer, resume infra in
`rust/crates/peerbeam-transfer/src/recover.rs`, first-class CLI) and in what
comparable apps do (LocalSend, PairDrop/Snapdrop, KDE Connect, Syncthing,
croc / magic-wormhole, Warpinator, AirDrop, Tailscale Taildrop).

**Working agreement:** a future session should implement these top-down. Each
item is a work order — build it with the full quality gate (fmt, clippy
`-D warnings`, tests, `flutter analyze`/`test`), live-verify, and commit per
milestone. **Do NOT build the excluded item.**

## ✅ Done since this roadmap

- **Send text / quick message** *(LocalSend-style)* — compose a message
  (type/paste), send to a picked device; receiver sees it in a **message
  dialog** with Copy (not a downloaded file). Transport still uses a tiny
  `.txt` under the hood (CLI-interop); a zero-file text frame is a possible
  future refinement.

---

## Tier 1 — flagship, on-mission

### 1. Relay + code-phrase transfer  *(croc / magic-wormhole)*
Reach any device over the internet with **no shared LAN or tailnet**: sender
gets a short phrase (e.g. `7-canyon-otter`), receiver enters it, a rendezvous
relay brokers an **end-to-end-encrypted** stream (relay never sees plaintext).
- **Why:** CLAUDE.md's route priority ends in "Direct Internet → Relay" and
  lists "Internet (future)." This is the missing pillar — turns PeerBeam from
  LAN/tailnet-only into "send to anyone."
- **Where:** new `peerbeam-relay` (server) + a `RelayTransport` /
  rendezvous client behind the existing `RouteManager`; slots in as the
  lowest-priority route. Keep the app-layer X25519 auth so the relay is
  untrusted.
- **Effort:** large (server + hosting + rendezvous protocol + NAT traversal
  fallback). **Payoff:** largest.
- **Done when:** two devices on different networks (no tailnet) transfer
  byte-exact via a phrase; relay sees only ciphertext.

### 2. Web receiver  *(PairDrop / Snapdrop)*
A device with **no app installed** opens a URL / scans a QR and receives in the
browser (WebRTC, or over the relay from #1).
- **Why:** kills "my friend has nothing installed" (hit live on Windows). Zero
  install.
- **Where:** small static web app + a WebRTC or relay bridge; reuses the
  transfer protocol/framing where possible.
- **Effort:** medium (rides on #1's relay). **Payoff:** high.
- **Done when:** a phone sends a file to a laptop that only opened a link.

---

## Tier 2 — strong, moderate effort

### 4. Resumable transfers surfaced in the UI
The engine already resumes after disconnect (`recover.rs`,
`send_file_recover`/`receive_file_recover`, checkpoint store). Expose it:
interrupted transfers show **Resume**, survive app restart, show partial
progress.
- **Effort:** medium (UI + FFI wiring; engine capability exists).
- **Done when:** kill Wi-Fi mid-transfer → reconnect → it resumes, not restarts.

### 5. Continuous folder sync  *(Syncthing-lite)*
Watch a folder and mirror changes to a peer.
- **Where:** CLI daemon fits best — `peerbeam sync ~/docs --to laptop`; later a
  GUI toggle.
- **Effort:** large (change detection, conflict handling, delta transfer).
- **Done when:** a file added on A appears on B without manual send.

### 6. Auto clipboard sync  *(KDE Connect)*
Opt-in: copy on A → paste on B automatically (vs today's manual clipboard send).
- **Where:** builds on existing clipboard plumbing + a live channel between
  trusted devices.
- **Effort:** medium. Privacy-gated (trusted + explicit opt-in only).

### 7. Trust hardening + optional PIN pairing  *(LocalSend)*
- Fix the known sharp edge: a peer is **pinned during the handshake even if the
  user declines** the transfer (`auth.rs` records before the accept gate in
  `transfer.rs`). Auto-accept then treats "connected once" as "approved." Make
  auto-accept require an actual **accept** (un-pin on decline, or track
  `approved` separately from the MITM key-pin).
- Add an optional **6-digit PIN** for first contact.
- **Effort:** small–medium. **Security-relevant** — prioritize within Tier 2.

---

## Tier 3 — polish / convenience

### 8. Desktop tray + global quick-send
Drag a file onto the tray/menu-bar icon → pick device. No window needed.

### 9. Send-to-self / "My devices"  *(AirDrop)*
Your own devices auto-grouped and auto-trusted; one tap.

### 10. Bandwidth limit
CLAUDE.md lists it in the transfer-engine requirements; not wired yet. A
throttle in the send loop + a Settings control.

### 11. Find / ring my device  *(KDE Connect)*
Buzz/notify a lost device on the tailnet.

### 12. Mobile "add Tailscale peer by address" hint
Android can't enumerate the tailnet (no CLI / sandboxed socket — platform
limit, see below). Add a one-line affordance in Nearby on mobile pointing to
**Saved devices → add by address** so tailnet reach is discoverable.

---

## Platform / known limitations (context, not tasks)

- **Android can't discover Tailscale peers** — the Tailscale app exposes no CLI
  and no reachable LocalAPI socket to a sandboxed app, so
  `peerbeam-discovery-tailscale` finds nothing there. Reach tailnet peers on
  mobile **by address** (Saved devices / QR). Not fixable in PeerBeam.
- **Signed installers** — macOS DMG / Windows MSIX are withheld until signing
  secrets exist; unsigned artifacts (Linux, Android, Windows portable zip, CLI)
  ship in CI. Windows GUI ships as a portable zip today.
- **Intel-mac CLI** binary not yet attached to releases (only `macos-arm64`);
  desktop app is universal.

## Already strong (don't re-add)

Tailscale-native reach, first-class CLI, QUIC + receiver-confirmed progress
with speed/ETA, folders + clipboard (app & CLI), QR share/scan, persistent
history/settings/trust with a management screen, auto-retry, instant cancel,
zero-config multi-transport.
