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
  is a follow-up.
- **Windows file permissions** not restricted (`0600` is Unix-only).

## Unverified in this environment (need hardware/toolchain)
- Real LAN/WAN throughput between two machines; Ethernet / USB-tethering /
  different-subnet transports; netem latency/loss + packet-reorder; 50/100 GB
  sustained transfers; sleep/wake; Android battery impact; Windows/macOS
  platform integration (notifications/tray/Finder/Wayland).

## Risks
- Beta-quality: core paths proven on Linux + Android real hardware, but desktop
  Windows/macOS + several transports/scale scenarios are unverified.
- No external security audit; TOFU first-contact is trust-on-first-use.
