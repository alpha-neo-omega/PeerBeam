# Compatibility & Feature Matrix (M5)

Legend: ✅ verified here · 🧪 covered by automated tests · 📱 verified on real
device · 🔧 code-complete, host/hardware needed to run · ➖ not applicable.

## Platform matrix
| Platform | Build | App runs | Engine live | Real transfer |
|---|---|---|---|---|
| Linux (x64) | ✅ | ✅ | ✅ | 📱 (Android→Linux, live) |
| Android (arm64) | ✅ (APK) | 📱 | 📱 (ports 49500+49600 bound) | 📱 (phone→Linux, byte-exact) |
| Windows | 🔧 (MSIX config) | 🔧 | 🔧 | 🔧 |
| macOS | 🔧 (DMG script + entitlements) | 🔧 | 🔧 | 🔧 |

## Feature matrix (engine)
| Feature | Status |
|---|---|
| Discovery (UDP/mDNS/Tailscale merge) | ✅ 🧪 📱 (Android↔Linux mutual) |
| Device status / online tracking | 🧪 |
| Send file | 🧪 📱 |
| Send folder | 🧪 (edge cases: unicode/long/hidden/deep) |
| Receive (+ accept/reject) | 🧪 📱 |
| Resume (after disconnect) | 🧪 (single-file + folder + recover driver) |
| Pause / Cancel | 🧪 |
| Progress + stats | 🧪 (FFI events) |
| History | 🧪 |
| Clipboard | 🧪 (local slot; network receive = follow-up) |
| Notifications | 📱 (Android foreground service) |
| Settings | 🧪 (versioned, persisted) |
| Daemon (start/stop/restart/status) | 🧪 |
| Logging | 🧪 (ring buffer + export) |

## Transport matrix
| Transport | Status |
|---|---|
| Loopback IPv4 | 🧪 |
| Loopback IPv6 (`::1`) | 🧪 |
| Wi-Fi | ✅ (self-transfer + Android↔Linux) |
| Tailscale | ✅ (self-transfer over tailscale0) |
| Ethernet | 🔧 (needs a wired peer) |
| USB tethering | 🔧 (needs a tethered peer) |
| Different subnets | 🔧 (netns — needs root) |
| Automatic route selection / failover / migration | 🧪 (RouteManager tests) |
| Reconnect + resume | 🧪 (recover driver) |

## Not runnable in this environment
- Windows/macOS native builds (no toolchain), a second physical machine for
  true cross-host LAN/Ethernet/USB/subnet transport, root for netem
  (latency/loss) + netns (subnets), sustained 50/100 GB on tmpfs.
