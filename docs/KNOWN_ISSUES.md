# Known Issues & Remaining Risks (M5)

## Gaps (not bugs — unbuilt / unwired)
- **Windows/macOS packages not built here** — config-complete (MSIX / DMG +
  entitlements); build on their hosts/CI.
- **Network clipboard receive** — FFI clipboard is a local slot + events;
  cross-device clipboard over the wire is a follow-up.
- **CLI `clipboard`/`history` + `daemon stop|status`** remain gated (exit 8);
  the engine features exist via FFI.
- **Folder send from the CLI** (`send <dir>`) not wired (engine supports it;
  FFI + Flutter do).
- **Settings apply on next init** — no live engine-mutation API.
- **Empty directories** are not recreated by folder transfer (walk is
  file-only). Symlinks are skipped (not followed).
- **Ephemeral identity** (CLI/FFI) → TOFU re-pins each run; persistent identity
  is a follow-up. A relaunch that reuses the same `app-<pid>` id but a fresh key
  is rejected as a key change — clear the peer's pin or use a fresh id.
- **Windows file permissions** not restricted (`0600` is Unix-only).
- **Tailscale discovery on Android** — tailnet peers do **not** appear in the
  Android app. Discovery needs `tailscaled`'s LocalAPI socket or the `tailscale`
  CLI; a sandboxed Android app can reach neither (the system Tailscale app is
  isolated, and there is no cross-app tailnet API). Desktop/CLI Tailscale
  discovery is unaffected. **Workaround:** use **Send to address** (Home → ⛃)
  with the peer's `100.x` Tailscale IP or MagicDNS name — the engine routes it
  over the tunnel. Real auto-discovery would require embedding Tailscale
  (`tsnet`/`libtailscale`), a large future item.

## Unverified in this environment (need hardware/toolchain)
- Real LAN/WAN throughput between two machines; Ethernet / USB-tethering /
  different-subnet transports; netem latency/loss + packet-reorder; 50/100 GB
  sustained transfers; sleep/wake; Android battery impact; Windows/macOS
  platform integration (notifications/tray/Finder/Wayland).

## Risks
- Beta-quality: core paths proven on Linux + Android real hardware, but desktop
  Windows/macOS + several transports/scale scenarios are unverified.
- No external security audit; TOFU first-contact is trust-on-first-use.
