# Final Compatibility Matrix — M8

Honest platform/transport support status at release-gate time. This supersedes
[Compatibility Matrix](COMPATIBILITY_MATRIX.md) for the M8 audit. **No result
is fabricated.** The audit environment is a single Linux host with an Android
device previously used for a live test; no Windows/macOS toolchain, no second
LAN host, no root for network-namespace simulation.

Legend: ✓ Verified (observed here or in a recorded live run) · 🟡 Code-reviewed
(builds/logic sound, not executed on that target) · ⚪ Environment-limited
(cannot be tested here).

## Platforms

| Platform | Build | Runtime | Transfer | Status |
|---|---|---|---|---|
| **Linux (x64)** | ✓ release CLI + FFI built | ✓ CLI runs, benchmark 726 MiB/s | ✓ loopback byte-exact | **Verified** |
| **Android** | 🟡 gradle config complete; debug-key APK builds | ✓ ran full engine on-device (prior) | ✓ live Android→Linux byte-exact (prior) | **Verified (prior live run)** |
| **Windows** | 🟡 packaging + MSIX config complete | ⚪ no host | ⚪ | **Env-limited** |
| **macOS** | 🟡 packaging config complete | ⚪ no host | ⚪ | **Env-limited** |

Notes:
- Android store-signed release requires `key.properties` (absent — secret);
  debug-signed build path verified in gradle config. ⚪ for store artifact.
- Windows/macOS have complete build/packaging scripts and Clean-Architecture
  code with no OS-specific blockers found in review, but were **not built or run**
  in this environment. Do not claim these as working until host-built.

## Transports

| Transport | Status | Basis |
|---|---|---|
| **Loopback (127.0.0.1)** | ✓ Verified | benchmark + example this session |
| **LAN (same subnet)** | ✓ Verified (prior) | live Android→Linux transfer |
| **Wi-Fi** | 🟡 Code-reviewed | same LAN path; prior live run was over local network |
| **Ethernet** | 🟡 Code-reviewed | same socket path as LAN; no separate NIC test here |
| **USB tethering** | 🟡 Code-reviewed | appears as a network interface; route logic covers it; not physically tested |
| **Tailscale** | 🟡 Code-reviewed | provider present (CLI + LocalAPI, MagicDNS); no tailnet exercised here |
| **IPv4** | ✓ Verified | all runs above |
| **IPv6** | 🟡 Code-reviewed | socket code is address-family agnostic; no IPv6 peer tested here |
| **Different subnets** | ⚪ Env-limited | requires routed multi-subnet setup / Tailscale peer |

## Route selection

🟡 `RouteManager` priority (LAN → USB → Ethernet → Wi-Fi → Tailscale →
DirectInternet → Relay) with failover/migration is code-reviewed and unit-tested
in logic, but automatic route **switching under real link loss** across distinct
physical transports was not exercised here (⚪).

## What "Verified" rests on

- Linux: commands run in this session (build, test, benchmark, example).
- Android + LAN: one recorded live transfer (Android→Linux, 72,322-byte JPEG,
  valid FFD8…FFD9, byte-exact, 0600, TOFU-pinned) from an earlier milestone.
  Reproduced conceptually by the loopback path this session, not re-run on
  hardware.

## Honest bottom line

**One platform (Linux) and one real cross-device path (Android↔Linux over LAN,
IPv4) are verified.** Everything else is code-reviewed or environment-limited.
Broad cross-platform/cross-transport claims are **not** substantiated in this
environment and must be validated on real Windows/macOS hosts and a
multi-transport network before a Stable v1.0 that advertises them.
